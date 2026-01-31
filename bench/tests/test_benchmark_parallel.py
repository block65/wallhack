"""Benchmark: parallel TCP streams through the tunnel."""

from __future__ import annotations

import pytest

from lib.constants import IP_TARGET, NS_CLIENT
from lib.iperf import Iperf3Server, run_iperf3_client

pytestmark = pytest.mark.benchmark


@pytest.mark.parametrize("streams", [1, 2, 3, 4, 5])
def test_tcp_parallel_streams(
    topology: None,
    iperf3_server: Iperf3Server,
    iperf3_bin: str,
    streams: int,
) -> None:
    """Test parallel TCP streams with varying counts."""
    result = run_iperf3_client(
        ns=NS_CLIENT,
        target_ip=IP_TARGET,
        binary=iperf3_bin,
        duration=3,
        parallel=streams,
    )
    assert result.bits_per_second > 0, f"No throughput with {streams} streams"
    print(f"\n  {streams} streams: {result.bits_per_second / 1e6:.2f} Mbps")
