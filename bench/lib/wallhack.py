"""Wallhack process management for benchmarks."""

from __future__ import annotations

import subprocess
import time
from pathlib import Path

from . import netns
from .constants import TUN_NAME, TUN_POLL_INTERVAL, TUN_READY_TIMEOUT


class WallhackProcess:
    """Manages a wallhack process running inside a network namespace."""

    def __init__(
        self,
        ns: str,
        args: list[str],
        binary: str | Path,
        env: dict[str, str] | None = None,
    ) -> None:
        self.ns = ns
        self.binary = str(binary)
        self.args = args
        self.env = env or {}
        self._proc: subprocess.Popen[bytes] | None = None

    def start(self, log_file: str | None = None) -> None:
        import os
        cmd = ["ip", "netns", "exec", self.ns, self.binary, *self.args]
        proc_env = os.environ.copy()
        proc_env.update(self.env)
        
        if log_file:
            self._log_file = open(log_file, "w")
            stdout_target = self._log_file
        else:
            self._log_file = None
            stdout_target = subprocess.PIPE
            
        self._proc = subprocess.Popen(
            cmd,
            stdin=subprocess.DEVNULL,
            stdout=stdout_target,
            stderr=subprocess.STDOUT,
            env=proc_env,
        )

    def stop(self) -> None:
        if self._proc is not None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait(timeout=5)
            self._proc = None
        if hasattr(self, '_log_file') and self._log_file:
            self._log_file.close()
            self._log_file = None

    def output(self) -> str:
        """Return whatever stdout/stderr the process has produced so far."""
        if self._proc is None or self._proc.stdout is None:
            return ""
        import selectors
        sel = selectors.DefaultSelector()
        sel.register(self._proc.stdout, selectors.EVENT_READ)
        chunks: list[bytes] = []
        while sel.select(timeout=0):
            data = self._proc.stdout.read1(4096)  # type: ignore[attr-defined]
            if not data:
                break
            chunks.append(data)
        sel.close()
        return b"".join(chunks).decode(errors="replace")

    def wait_for_tun(self, ns: str | None = None, tun_name: str | None = None) -> None:
        """Wait for wallhack to create the TUN, then bring it UP.

        Wallhack creates the TUN in DOWN state when an exit node connects.
        The operator is responsible for bringing it up and adding routes.
        """
        target_ns = ns or self.ns
        target_tun = tun_name or TUN_NAME
        deadline = time.monotonic() + TUN_READY_TIMEOUT
        while time.monotonic() < deadline:
            # Check if wallhack crashed
            if self._proc is not None and self._proc.poll() is not None:
                out = self.output()
                raise RuntimeError(
                    f"wallhack exited with code {self._proc.returncode} "
                    f"before TUN appeared:\n{out}"
                )
            if netns.link_exists(target_ns, target_tun):
                # Wallhack created it in DOWN state -- bring it up
                netns.set_link_up(target_ns, target_tun)
                return
            time.sleep(TUN_POLL_INTERVAL)
        out = self.output()
        raise TimeoutError(
            f"TUN interface {target_tun} did not appear in {target_ns} "
            f"within {TUN_READY_TIMEOUT}s\nwallhack output:\n{out}"
        )

    @property
    def pid(self) -> int | None:
        return self._proc.pid if self._proc else None
