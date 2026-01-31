"""Simple UDP tests through the tunnel using Python sockets (no iperf3)."""

from __future__ import annotations

import subprocess
import textwrap

import pytest

from lib.constants import IP_TARGET, NS_CLIENT, NS_TARGET

pytestmark = pytest.mark.smoketest

UDP_PORT = 9998


def _udp_echo_test(payloads: list[bytes], timeout: float = 5.0) -> list[bytes]:
    """Send multiple UDP packets and receive echoes.
    
    Runs server in target namespace, client in client namespace.
    Returns list of received responses.
    """
    payloads_hex = [p.hex() for p in payloads]
    
    # Server script - receives packets and echoes them back
    server_script = textwrap.dedent(f"""\
        import socket
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(('0.0.0.0', {UDP_PORT}))
        s.settimeout({timeout})
        for _ in range({len(payloads)}):
            data, addr = s.recvfrom(4096)
            s.sendto(data, addr)
        s.close()
    """)
    
    # Client script - sends packets and receives echoes
    client_script = textwrap.dedent(f"""\
        import socket
        payloads = {payloads_hex!r}
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.settimeout({timeout})
        results = []
        for p in payloads:
            s.sendto(bytes.fromhex(p), ('{IP_TARGET}', {UDP_PORT}))
            data, _ = s.recvfrom(4096)
            results.append(data.hex())
        s.close()
        print('|'.join(results), end='')
    """)
    
    # Start server
    server = subprocess.Popen(
        ["ip", "netns", "exec", NS_TARGET, "python3", "-c", server_script],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    
    try:
        # Run client
        result = subprocess.run(
            ["ip", "netns", "exec", NS_CLIENT, "python3", "-c", client_script],
            capture_output=True,
            text=True,
            timeout=timeout + 2,
        )
        assert result.returncode == 0, f"Client failed: {result.stderr}"
        
        # Parse results
        if not result.stdout:
            return []
        return [bytes.fromhex(h) for h in result.stdout.split('|')]
    finally:
        server.terminate()
        try:
            server.wait(timeout=1)
        except subprocess.TimeoutExpired:
            server.kill()


def test_udp_single_packet(topology) -> None:
    """Send a single UDP packet through the tunnel."""
    payload = b"PING"
    results = _udp_echo_test([payload])
    assert results == [payload]


def test_udp_three_packets(topology) -> None:
    """Send three UDP packets."""
    payloads = [f"PKT{i}".encode() for i in range(3)]
    results = _udp_echo_test(payloads)
    assert results == payloads


def test_udp_ten_packets(topology) -> None:
    """Send ten UDP packets."""
    payloads = [f"DATA{i:02d}".encode() for i in range(10)]
    results = _udp_echo_test(payloads)
    assert results == payloads


def test_udp_hundred_packets(topology) -> None:
    """Send 100 UDP packets."""
    payloads = [f"MSG{i:03d}".encode() for i in range(100)]
    results = _udp_echo_test(payloads)
    assert results == payloads
