"""Benchmark: RTT latency through QUIC vs WebSocket tunnel.

Uses ping (ICMP) through the wallhack TUN interface to measure round-trip
latency for each transport. ICMP is proxied by wallhack on Linux via the
tunnel stack (entry → smoltcp → transport → exit → target).

Run only the latency tests:
    sudo pytest -m benchmark -k latency -v

Run both transports and compare output manually.
"""

from __future__ import annotations

import pytest

from lib.constants import IP_TARGET, NS_CLIENT
from lib.ping import run_ping


pytestmark = pytest.mark.benchmark

# Sanity ceiling: loopback through netns + tunnel should be well under this.
MAX_ACCEPTABLE_LATENCY_MS = 100.0
# Treat >5% packet loss as a failure.
MAX_ACCEPTABLE_LOSS_PERCENT = 5.0


def test_quic_latency(topology: None) -> None:
    """Measure RTT latency through the QUIC (UDP) tunnel."""
    result = run_ping(ns=NS_CLIENT, target_ip=IP_TARGET, count=50, interval=0.1)

    assert result.received > 0, (
        f"All {result.transmitted} pings lost — ICMP not reaching target over QUIC"
    )
    assert result.loss_percent <= MAX_ACCEPTABLE_LOSS_PERCENT, (
        f"QUIC tunnel packet loss too high: {result.loss_percent:.1f}%"
    )
    assert result.avg_ms < MAX_ACCEPTABLE_LATENCY_MS, (
        f"QUIC average RTT too high: {result.avg_ms:.3f} ms"
    )

    print(
        f"\n  QUIC  RTT: min={result.min_ms:.3f}ms  avg={result.avg_ms:.3f}ms"
        f"  max={result.max_ms:.3f}ms  mdev={result.mdev_ms:.3f}ms"
        f"  loss={result.loss_percent:.1f}%"
    )


def test_websocket_latency(topology_websocket: None) -> None:
    """Measure RTT latency through the WebSocket (TCP) tunnel."""
    result = run_ping(ns=NS_CLIENT, target_ip=IP_TARGET, count=50, interval=0.1)

    assert result.received > 0, (
        f"All {result.transmitted} pings lost — ICMP not reaching target over WebSocket"
    )
    assert result.loss_percent <= MAX_ACCEPTABLE_LOSS_PERCENT, (
        f"WebSocket tunnel packet loss too high: {result.loss_percent:.1f}%"
    )
    assert result.avg_ms < MAX_ACCEPTABLE_LATENCY_MS, (
        f"WebSocket average RTT too high: {result.avg_ms:.3f} ms"
    )

    print(
        f"\n  WS    RTT: min={result.min_ms:.3f}ms  avg={result.avg_ms:.3f}ms"
        f"  max={result.max_ms:.3f}ms  mdev={result.mdev_ms:.3f}ms"
        f"  loss={result.loss_percent:.1f}%"
    )
