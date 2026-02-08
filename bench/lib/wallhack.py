"""Wallhack process management for benchmarks."""

from __future__ import annotations

import os
import subprocess
import time
from pathlib import Path

from . import netns
from .constants import TUN_NAME, TUN_POLL_INTERVAL, TUN_READY_TIMEOUT


def _fmt_bytes(n: int) -> str:
    if n >= 1_048_576:
        return f"{n / 1_048_576:.1f} MB"
    if n >= 1_024:
        return f"{n / 1_024:.1f} KB"
    return f"{n} B"


class WallhackProcess:
    """Manages a wallhack process running inside a network namespace.

    When ``memory_limit`` is set (e.g. ``"256M"``), the process is wrapped in
    a ``systemd-run --scope`` with ``MemoryMax`` so the cgroup OOM killer
    targets only this process — not the whole machine.
    """

    def __init__(
        self,
        ns: str,
        args: list[str],
        binary: str | Path,
        env: dict[str, str] | None = None,
        memory_limit: str | None = None,
    ) -> None:
        self.ns = ns
        self.binary = str(binary)
        self.args = args
        self.env = env or {}
        self.memory_limit = memory_limit
        self._proc: subprocess.Popen[bytes] | None = None
        self._log_file = None
        self._scope_name: str | None = None

    def start(self, log_file: str | None = None) -> None:
        cmd: list[str] = ["ip", "netns", "exec", self.ns, self.binary, *self.args]

        if self.memory_limit:
            # Unique scope name: pid + object id avoids collisions
            self._scope_name = f"wh-bench-{os.getpid()}-{id(self):x}"
            cmd = [
                "systemd-run", "--scope",
                f"--unit={self._scope_name}",
                "-p", f"MemoryMax={self.memory_limit}",
                "-q", "--",
                *cmd,
            ]

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
        # Report peak memory before tearing down
        peak = self.peak_rss()
        if peak is not None:
            limit_str = f" / limit {self.memory_limit}" if self.memory_limit else ""
            print(f"  [memory] peak RSS: {_fmt_bytes(peak)}{limit_str}")

        if self._proc is not None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait(timeout=5)
            self._proc = None
        if self._log_file:
            self._log_file.close()
            self._log_file = None

    # ----- cgroup memory stats -----

    def _cgroup_path(self) -> Path | None:
        """Return the cgroup v2 directory for this process's scope."""
        if not self._scope_name:
            return None
        return Path(f"/sys/fs/cgroup/system.slice/{self._scope_name}.scope")

    def peak_rss(self) -> int | None:
        """Peak memory usage (bytes) from the cgroup's ``memory.peak``."""
        cg = self._cgroup_path()
        if cg is None:
            return self._peak_rss_proc()
        try:
            return int((cg / "memory.peak").read_text().strip())
        except (FileNotFoundError, ValueError, PermissionError):
            return self._peak_rss_proc()

    def memory_current(self) -> int | None:
        """Current memory usage (bytes) from the cgroup's ``memory.current``."""
        cg = self._cgroup_path()
        if cg is None:
            return None
        try:
            return int((cg / "memory.current").read_text().strip())
        except (FileNotFoundError, ValueError, PermissionError):
            return None

    def _peak_rss_proc(self) -> int | None:
        """Fallback: read VmHWM from /proc/{pid}/status."""
        if self._proc is None:
            return None
        try:
            status = Path(f"/proc/{self._proc.pid}/status").read_text()
            for line in status.splitlines():
                if line.startswith("VmHWM:"):
                    return int(line.split()[1]) * 1024  # kB -> bytes
        except (FileNotFoundError, ValueError, PermissionError):
            pass
        return None

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
