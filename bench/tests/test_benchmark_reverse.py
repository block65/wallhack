"""Benchmark: reverse mode (server -> client) through the tunnel."""

from __future__ import annotations

import pytest

from lib.constants import IP_TARGET, NS_CLIENT
from lib.iperf import Iperf3Server, run_iperf3_client

pytestmark = pytest.mark.benchmark


def test_tcp_reverse(
    topology: None,
    iperf3_server: Iperf3Server,
    iperf3_bin: str,
) -> None:
    """TCP reverse mode: server sends to client."""
    result = run_iperf3_client(
        ns=NS_CLIENT,
        target_ip=IP_TARGET,
        binary=iperf3_bin,
        duration=5,
        reverse=True,
    )
    assert result.bits_per_second > 0, "No throughput measured"
    print(f"\n  reverse throughput: {result.bits_per_second / 1e6:.2f} Mbps")
