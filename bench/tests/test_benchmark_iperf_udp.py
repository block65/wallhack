"""Benchmark: iperf3 UDP bandwidth ramp with loss and jitter metrics."""

from __future__ import annotations

import pytest

from lib.constants import IP_TARGET, NS_CLIENT
from lib.iperf import Iperf3Server, IperfResult, run_iperf3_client

pytestmark = pytest.mark.benchmark

BANDWIDTHS = ["100K", "500K", "1M", "5M", "10M"]


@pytest.mark.parametrize("bandwidth", BANDWIDTHS)
def test_udp_bandwidth(
    topology: None,
    iperf3_server: Iperf3Server,
    iperf3_bin: str,
    bandwidth: str,
) -> None:
    """UDP bandwidth test at target rate with loss/jitter metrics."""
    result = run_iperf3_client(
        ns=NS_CLIENT,
        target_ip=IP_TARGET,
        binary=iperf3_bin,
        duration=2,
        bandwidth=bandwidth,
        udp=True,
    )
    assert result.bits_per_second > 0, "No throughput measured"
    # Log metrics for visibility (captured by pytest)
    print(f"\n  throughput: {result.bits_per_second / 1e6:.2f} Mbps")
    print(f"  loss: {result.lost_percent:.2f}%")
    print(f"  jitter: {result.jitter_ms:.3f} ms")
