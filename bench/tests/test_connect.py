"""Pre-smoketest: verify wallhack entry + exit can connect and create a TUN.

No network namespaces needed -- runs on localhost.
"""

from __future__ import annotations

import subprocess
import time

import pytest

from lib.constants import (
    PEER_NAME,
    PROCESS_STARTUP_DELAY,
    TUN_NAME,
    WALLHACK_LISTEN_PORT,
)
from lib.netns import link_exists, set_link_up

pytestmark = pytest.mark.connect


def _start_wallhack(args: list[str], binary: str) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [binary, *args],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


def _read_output(proc: subprocess.Popen[bytes]) -> str:
    if proc.stdout is None:
        return ""
    import selectors
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ)
    chunks: list[bytes] = []
    while sel.select(timeout=0):
        data = proc.stdout.read1(4096)  # type: ignore[attr-defined]
        if not data:
            break
        chunks.append(data)
    sel.close()
    return b"".join(chunks).decode(errors="replace")


def _stop(proc: subprocess.Popen[bytes]) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def test_entry_starts(wallhack_bin: str) -> None:
    """Entry node starts and listens without crashing."""
    proc = _start_wallhack(["entry", "-l", f":{WALLHACK_LISTEN_PORT}", "-v"], wallhack_bin)
    time.sleep(PROCESS_STARTUP_DELAY)
    try:
        assert proc.poll() is None, (
            f"entry exited with code {proc.returncode}:\n{_read_output(proc)}"
        )
    finally:
        _stop(proc)


def test_exit_connects(wallhack_bin: str) -> None:
    """Exit node connects to entry node without crashing."""
    entry = _start_wallhack(["entry", "-l", f":{WALLHACK_LISTEN_PORT}", "-v"], wallhack_bin)
    time.sleep(PROCESS_STARTUP_DELAY)

    exit_proc = _start_wallhack(
        ["exit", "-c", f"127.0.0.1:{WALLHACK_LISTEN_PORT}", "--name", PEER_NAME, "-v"],
        wallhack_bin,
    )
    time.sleep(PROCESS_STARTUP_DELAY * 2)

    try:
        assert entry.poll() is None, f"entry crashed:\n{_read_output(entry)}"
        assert exit_proc.poll() is None, f"exit crashed:\n{_read_output(exit_proc)}"
    finally:
        _stop(exit_proc)
        _stop(entry)


def test_tun_created(wallhack_bin: str) -> None:
    """TUN interface appears after exit connects to entry."""
    entry = _start_wallhack(["entry", "-l", f":{WALLHACK_LISTEN_PORT}", "-v"], wallhack_bin)
    time.sleep(PROCESS_STARTUP_DELAY)

    exit_proc = _start_wallhack(
        ["exit", "-c", f"127.0.0.1:{WALLHACK_LISTEN_PORT}", "--name", PEER_NAME, "-v"],
        wallhack_bin,
    )

    try:
        # Poll for TUN creation (up to 15s)
        deadline = time.monotonic() + 15
        found = False
        while time.monotonic() < deadline:
            if entry.poll() is not None:
                pytest.fail(f"entry crashed:\n{_read_output(entry)}")
            if exit_proc.poll() is not None:
                pytest.fail(f"exit crashed:\n{_read_output(exit_proc)}")
            if link_exists("", TUN_NAME):
                set_link_up("", TUN_NAME)
                found = True
                break
            time.sleep(0.5)

        # Dump output for debugging
        print(f"\n--- entry output ---\n{_read_output(entry)}")
        print(f"\n--- exit output ---\n{_read_output(exit_proc)}")

        # List all tun interfaces
        result = subprocess.run(
            ["ip", "link", "show"], capture_output=True, text=True, check=False,
        )
        print(f"\n--- interfaces ---\n{result.stdout}")

        assert found, f"{TUN_NAME} not found within 15s"
    finally:
        _stop(exit_proc)
        _stop(entry)
