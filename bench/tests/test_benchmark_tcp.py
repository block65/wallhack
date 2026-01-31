"""Benchmark: progressive TCP payload sizes through the tunnel."""

from __future__ import annotations

import subprocess
import textwrap

import pytest

from lib.constants import ECHO_PORT, IP_TARGET, NS_CLIENT

pytestmark = pytest.mark.benchmark


@pytest.mark.parametrize(
    "size",
    [1, 10, 100, 1_000, 10_000, 100_000, 1_000_000],
    ids=["1B", "10B", "100B", "1KB", "10KB", "100KB", "1MB"],
)
def test_tcp_echo_payload(topology: None, size: int) -> None:
    """Send a payload of `size` bytes through the tunnel and verify echo."""
    # Generate deterministic payload
    payload = bytes(i % 256 for i in range(size))

    script = textwrap.dedent(f"""\
        import socket, hashlib
        payload = bytes(i % 256 for i in range({size}))
        expected = hashlib.sha256(payload).hexdigest()
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(30)
        s.connect(("{IP_TARGET}", {ECHO_PORT}))
        s.sendall(payload)
        buf = b""
        while len(buf) < {size}:
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
        s.close()
        actual = hashlib.sha256(buf).hexdigest()
        assert actual == expected, f"hash mismatch: {{actual}} != {{expected}}"
        print("OK", end="")
    """)
    result = subprocess.run(
        ["ip", "netns", "exec", NS_CLIENT, "python3", "-c", script],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0, f"echo client failed: {result.stderr}"
    assert result.stdout == "OK"
