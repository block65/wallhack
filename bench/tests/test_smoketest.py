"""Smoketest: basic tunnel connectivity tests (no iperf3 required)."""

from __future__ import annotations

import subprocess
import textwrap

import pytest

from lib.constants import ECHO_PORT, IP_TARGET, NS_CLIENT

pytestmark = pytest.mark.smoketest


def _tcp_echo_from_client(payload_hex: str, length: int) -> str:
    """Run a self-contained TCP echo client inside wh-client namespace.

    Takes hex-encoded payload, returns hex-encoded response.
    """
    script = textwrap.dedent(f"""\
        import socket
        payload = bytes.fromhex("{payload_hex}")
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(10)
        s.connect(("{IP_TARGET}", {ECHO_PORT}))
        s.sendall(payload)
        buf = b""
        while len(buf) < {length}:
            chunk = s.recv(4096)
            if not chunk:
                break
            buf += chunk
        s.close()
        print(buf.hex(), end="")
    """)
    result = subprocess.run(
        ["ip", "netns", "exec", NS_CLIENT, "python3", "-c", script],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, f"echo client failed: {result.stderr}"
    return result.stdout


def test_single_byte_tcp(topology) -> None:
    """Send a single byte through the tunnel and verify echo."""
    payload = b"\x42"
    try:
        response_hex = _tcp_echo_from_client(payload.hex(), len(payload))
    except (AssertionError, subprocess.TimeoutExpired) as e:
        print(f"\n{topology.dump()}")
        raise
    assert response_hex == payload.hex()


def test_tcp_echo_hello(topology) -> None:
    """Send 'hello world' through the tunnel and verify echo."""
    payload = b"hello world"
    try:
        response_hex = _tcp_echo_from_client(payload.hex(), len(payload))
    except (AssertionError, subprocess.TimeoutExpired) as e:
        print(f"\n{topology.dump()}")
        raise
    assert response_hex == payload.hex()


def test_tcp_echo_binary_payload(topology) -> None:
    """Send all 256 byte values through the tunnel and verify echo."""
    payload = bytes(range(256))
    try:
        response_hex = _tcp_echo_from_client(payload.hex(), len(payload))
    except (AssertionError, subprocess.TimeoutExpired) as e:
        print(f"\n{topology.dump()}")
        raise
    assert response_hex == payload.hex()
