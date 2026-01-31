"""Pure-stdlib TCP echo server for running inside a network namespace."""

from __future__ import annotations

import subprocess
import textwrap

from .constants import ECHO_PORT


# Self-contained Python TCP echo server script (no external deps).
# Runs as a subprocess via `ip netns exec`.
_ECHO_SERVER_SCRIPT = textwrap.dedent(f"""\
    import socket, threading, signal, sys

    def handle(conn):
        try:
            while True:
                data = conn.recv(4096)
                if not data:
                    break
                conn.sendall(data)
        finally:
            conn.close()

    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("0.0.0.0", {ECHO_PORT}))
    sock.listen(32)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))

    while True:
        conn, _ = sock.accept()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()
""")


class EchoServer:
    """TCP echo server running inside a network namespace."""

    def __init__(self, ns: str) -> None:
        self.ns = ns
        self._proc: subprocess.Popen[bytes] | None = None

    def start(self) -> None:
        self._proc = subprocess.Popen(
            ["ip", "netns", "exec", self.ns, "python3", "-c", _ECHO_SERVER_SCRIPT],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        # Give it a moment to start, then verify it's running
        import time
        time.sleep(0.5)
        if self._proc.poll() is not None:
            stderr = self._proc.stderr.read().decode() if self._proc.stderr else ""
            raise RuntimeError(f"Echo server died on startup: {stderr}")

    def stop(self) -> None:
        if self._proc is not None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait(timeout=5)
            self._proc = None
