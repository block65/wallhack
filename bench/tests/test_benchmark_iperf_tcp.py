"""Benchmark: iperf3 TCP bandwidth ramp through the tunnel."""

from __future__ import annotations

import pytest

from lib.constants import IP_TARGET, NS_CLIENT
from lib.iperf import Iperf3Server, IperfResult, run_iperf3_client

pytestmark = pytest.mark.benchmark

BANDWIDTHS = ["100K", "500K", "1M", "5M", "10M", "0"]
BANDWIDTH_IDS = ["100K", "500K", "1M", "5M", "10M", "unlimited"]


@pytest.mark.parametrize("bandwidth", BANDWIDTHS, ids=BANDWIDTH_IDS)
def test_tcp_bandwidth(
    topology: None,
    iperf3_server: Iperf3Server,
    iperf3_bin: str,
    bandwidth: str,
) -> None:
    """TCP bandwidth test at target rate."""
    bw = bandwidth if bandwidth != "0" else None
    result = run_iperf3_client(
        ns=NS_CLIENT,
        target_ip=IP_TARGET,
        binary=iperf3_bin,
        duration=5,
        bandwidth=bw,
    )
    assert result.bits_per_second > 0, "No throughput measured"
