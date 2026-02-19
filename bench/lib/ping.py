"""ping runner and RTT output parser."""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass


@dataclass
class PingResult:
    """Parsed ping RTT statistics."""

    min_ms: float = 0.0
    avg_ms: float = 0.0
    max_ms: float = 0.0
    mdev_ms: float = 0.0
    transmitted: int = 0
    received: int = 0
    loss_percent: float = 0.0


# rtt min/avg/max/mdev = 0.123/0.456/0.789/0.012 ms
_RTT_RE = re.compile(
    r"rtt min/avg/max/mdev = ([\d.]+)/([\d.]+)/([\d.]+)/([\d.]+) ms"
)
# N packets transmitted, M received, X% packet loss
_SUMMARY_RE = re.compile(
    r"(\d+) packets transmitted, (\d+) received, ([\d.]+)% packet loss"
)


def run_ping(
    ns: str,
    target_ip: str,
    count: int = 50,
    interval: float = 0.1,
) -> PingResult:
    """Run ping inside a network namespace and return parsed RTT stats.

    Args:
        ns: Network namespace name (e.g. "wh-client").
        target_ip: Destination IP address.
        count: Number of ICMP echo requests to send.
        interval: Seconds between pings (0.1 = 100ms, minimum without root is 0.2).
    """
    timeout = int(count * interval) + 10
    cmd = [
        "ip", "netns", "exec", ns,
        "ping",
        "-c", str(count),
        "-i", str(interval),
        "-q",  # quiet: only print summary
        target_ip,
    ]
    proc = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    return _parse_ping_output(proc.stdout + proc.stderr)


def _parse_ping_output(output: str) -> PingResult:
    result = PingResult()

    m = _SUMMARY_RE.search(output)
    if m:
        result.transmitted = int(m.group(1))
        result.received = int(m.group(2))
        result.loss_percent = float(m.group(3))

    m = _RTT_RE.search(output)
    if m:
        result.min_ms = float(m.group(1))
        result.avg_ms = float(m.group(2))
        result.max_ms = float(m.group(3))
        result.mdev_ms = float(m.group(4))

    return result
