"""Benchmark: TCP throughput through WebSocket tunnel."""

from __future__ import annotations

import time

import pytest

from lib.constants import IP_TARGET, NS_CLIENT, NS_TARGET, PROCESS_STARTUP_DELAY
from lib.iperf import Iperf3Server, run_iperf3_client


pytestmark = pytest.mark.benchmark


@pytest.fixture
def iperf3_server_ws(topology_websocket: None, iperf3_bin: str) -> Iperf3Server:
    """Start an iperf3 server for websocket tests."""
    server = Iperf3Server(NS_TARGET, binary=iperf3_bin)
    server.start()
    time.sleep(PROCESS_STARTUP_DELAY)
    try:
        yield server
    finally:
        server.stop()
        time.sleep(0.5)


def test_websocket_tcp_throughput(
    topology_websocket: None,
    iperf3_server_ws: Iperf3Server,
    iperf3_bin: str,
) -> None:
    """Single TCP stream through WebSocket tunnel."""
    result = run_iperf3_client(
        ns=NS_CLIENT,
        target_ip=IP_TARGET,
        binary=iperf3_bin,
        duration=5,
    )
    assert result.bits_per_second > 0, "No throughput measured"
    print(f"\n  WebSocket TCP: {result.bits_per_second / 1e6:.2f} Mbps")


@pytest.mark.parametrize("streams", [1, 2, 3, 4])
def test_websocket_parallel_streams(
    topology_websocket: None,
    iperf3_server_ws: Iperf3Server,
    iperf3_bin: str,
    streams: int,
) -> None:
    """Test parallel TCP streams through WebSocket tunnel."""
    result = run_iperf3_client(
        ns=NS_CLIENT,
        target_ip=IP_TARGET,
        binary=iperf3_bin,
        duration=3,
        parallel=streams,
    )
    assert result.bits_per_second > 0, f"No throughput with {streams} streams"
    print(f"\n  WebSocket {streams} streams: {result.bits_per_second / 1e6:.2f} Mbps")
