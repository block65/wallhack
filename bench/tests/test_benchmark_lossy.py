"""Benchmark: QUIC vs WebSocket throughput under simulated packet loss.

Netem is applied symmetrically to both ends of the entry↔exit tunnel link.

IMPORTANT: QUIC and WebSocket tests must be run in separate pytest sessions
(see justfile: benchmark-lossy-quic / benchmark-lossy-ws). The WebSocket
tunnel uses TCP: sustained netem can drop the tunnel connection, causing all
subsequent WebSocket tests in the same session to fail.

The parallel-streams tests are the key differentiator: with QUIC each iperf3
TCP connection maps to an independent QUIC stream, so a lost packet only stalls
that one stream. Over WebSocket (yamux-over-TCP) all connections share one TCP
flow — a lost packet stalls all of them until TCP retransmits.

Run only QUIC lossy tests:
    just benchmark-lossy-quic

Run only WebSocket lossy tests:
    just benchmark-lossy-ws
"""

from __future__ import annotations

import time

import pytest

from lib.constants import (
    IP_TARGET,
    NS_CLIENT,
    NS_ENTRY,
    NS_EXIT,
    NS_TARGET,
    PROCESS_STARTUP_DELAY,
    VETH_EE_ENTRY,
    VETH_EE_EXIT,
)
from lib.iperf import Iperf3Server, run_iperf3_client
from lib.netns import clear_netem, set_netem


pytestmark = pytest.mark.benchmark

# (loss_pct, one_way_delay_ms)
# Delay is applied to both sides so added RTT = 2 * delay_ms.
# Severe (5% / 50ms) is intentionally omitted: the WebSocket tunnel TCP
# connection drops under those conditions, making all subsequent WS tests fail.
SCENARIOS = [
    pytest.param((0.5,  5), id="light"),     # 0.5% loss, +10ms RTT
    pytest.param((2.0, 25), id="moderate"),  # 2.0% loss, +50ms RTT
]


@pytest.fixture
def netem(request):
    """Apply symmetric netem to the tunnel link for the duration of the test."""
    loss_pct, delay_ms = request.param
    set_netem(NS_ENTRY, VETH_EE_ENTRY, loss_pct, delay_ms)
    set_netem(NS_EXIT, VETH_EE_EXIT, loss_pct, delay_ms)
    yield {"loss_pct": loss_pct, "delay_ms": delay_ms}
    clear_netem(NS_ENTRY, VETH_EE_ENTRY)
    clear_netem(NS_EXIT, VETH_EE_EXIT)
    time.sleep(1)  # let TCP recover before the next test


@pytest.fixture
def iperf3_server_quic(topology, iperf3_bin):
    server = Iperf3Server(NS_TARGET, binary=iperf3_bin)
    server.start()
    time.sleep(PROCESS_STARTUP_DELAY)
    try:
        yield server
    finally:
        server.stop()
        time.sleep(0.5)


@pytest.fixture
def iperf3_server_ws(topology_websocket, iperf3_bin):
    server = Iperf3Server(NS_TARGET, binary=iperf3_bin)
    server.start()
    time.sleep(PROCESS_STARTUP_DELAY)
    try:
        yield server
    finally:
        server.stop()
        time.sleep(0.5)


# ---------------------------------------------------------------------------
# Single-stream throughput under loss
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("netem", SCENARIOS, indirect=True)
def test_quic_lossy_throughput(topology, iperf3_server_quic, iperf3_bin, netem):
    result = run_iperf3_client(
        ns=NS_CLIENT, target_ip=IP_TARGET, binary=iperf3_bin, duration=10,
    )
    mbps = result.bits_per_second / 1e6
    print(
        f"\n  QUIC  [{netem['loss_pct']}% loss +{netem['delay_ms']*2}ms RTT]"
        f"  {mbps:.1f} Mbps  retransmits={result.retransmits}"
    )
    assert result.bits_per_second > 0, "No throughput over QUIC (tunnel timed out)"


@pytest.mark.parametrize("netem", SCENARIOS, indirect=True)
def test_websocket_lossy_throughput(topology_websocket, iperf3_server_ws, iperf3_bin, netem):
    result = run_iperf3_client(
        ns=NS_CLIENT, target_ip=IP_TARGET, binary=iperf3_bin, duration=10,
    )
    mbps = result.bits_per_second / 1e6
    print(
        f"\n  WS    [{netem['loss_pct']}% loss +{netem['delay_ms']*2}ms RTT]"
        f"  {mbps:.1f} Mbps  retransmits={result.retransmits}"
    )
    assert result.bits_per_second > 0, "No throughput over WebSocket (tunnel timed out)"


# ---------------------------------------------------------------------------
# Parallel streams — the head-of-line blocking test
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("netem", SCENARIOS, indirect=True)
@pytest.mark.parametrize("streams", [4])
def test_quic_lossy_parallel(topology, iperf3_server_quic, iperf3_bin, netem, streams):
    """4 parallel TCP streams over QUIC: each gets an independent QUIC stream,
    so packet loss on one does not stall the others."""
    result = run_iperf3_client(
        ns=NS_CLIENT, target_ip=IP_TARGET, binary=iperf3_bin,
        duration=10, parallel=streams,
    )
    mbps = result.bits_per_second / 1e6
    print(
        f"\n  QUIC  [{netem['loss_pct']}% loss +{netem['delay_ms']*2}ms RTT]"
        f"  {streams}x parallel  {mbps:.1f} Mbps"
    )
    assert result.bits_per_second > 0, "No throughput over QUIC (tunnel timed out)"


@pytest.mark.parametrize("netem", SCENARIOS, indirect=True)
@pytest.mark.parametrize("streams", [4])
def test_websocket_lossy_parallel(topology_websocket, iperf3_server_ws, iperf3_bin, netem, streams):
    """4 parallel TCP streams over WebSocket: all share one TCP/yamux flow,
    so a single lost packet stalls every stream until TCP retransmits."""
    result = run_iperf3_client(
        ns=NS_CLIENT, target_ip=IP_TARGET, binary=iperf3_bin,
        duration=10, parallel=streams,
    )
    mbps = result.bits_per_second / 1e6
    print(
        f"\n  WS    [{netem['loss_pct']}% loss +{netem['delay_ms']*2}ms RTT]"
        f"  {streams}x parallel  {mbps:.1f} Mbps"
    )
    assert result.bits_per_second > 0, "No throughput over WebSocket (tunnel timed out)"
