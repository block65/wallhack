"""Benchmark: parallel TCP streams through the tunnel."""

from __future__ import annotations

import pytest

from lib.constants import IP_TARGET, NS_CLIENT
from lib.iperf import Iperf3Server, run_iperf3_client

pytestmark = pytest.mark.benchmark


def test_tcp_parallel_4_streams(
    topology: None,
    iperf3_server: Iperf3Server,
    iperf3_bin: str,
) -> None:
    """4-way parallel TCP streams."""
    result = run_iperf3_client(
        ns=NS_CLIENT,
        target_ip=IP_TARGET,
        binary=iperf3_bin,
        duration=5,
        parallel=4,
    )
    assert result.bits_per_second > 0, "No throughput measured"
    print(f"\n  aggregate: {result.bits_per_second / 1e6:.2f} Mbps")
