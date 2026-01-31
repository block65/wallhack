"""iperf3 runner and JSON output parser."""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from .constants import IPERF3_PORT


@dataclass
class IperfResult:
    """Parsed iperf3 JSON output."""

    bits_per_second: float = 0.0
    retransmits: int = 0
    lost_percent: float = 0.0
    jitter_ms: float = 0.0
    raw: dict = field(default_factory=dict)


class Iperf3Server:
    """iperf3 server running inside a network namespace."""

    def __init__(self, ns: str, binary: str | Path = "iperf3") -> None:
        self.ns = ns
        self.binary = str(binary)
        self._proc: subprocess.Popen[bytes] | None = None

    def start(self) -> None:
        self._proc = subprocess.Popen(
            [
                "ip", "netns", "exec", self.ns,
                self.binary, "--server", "--one-off", "--port", str(IPERF3_PORT),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def stop(self) -> None:
        if self._proc is not None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait(timeout=5)
            self._proc = None


def run_iperf3_client(
    ns: str,
    target_ip: str,
    binary: str | Path = "iperf3",
    duration: int = 10,
    bandwidth: str | None = None,
    udp: bool = False,
    parallel: int = 1,
    reverse: bool = False,
) -> IperfResult:
    """Run iperf3 client in a namespace, return parsed results."""
    cmd = [
        "ip", "netns", "exec", ns,
        str(binary),
        "-c", target_ip,
        "-t", str(duration),
        "-p", str(IPERF3_PORT),
        "-J",  # JSON output
    ]

    if bandwidth:
        cmd.extend(["-b", bandwidth])
    if udp:
        cmd.append("-u")
    if parallel > 1:
        cmd.extend(["-P", str(parallel)])
    if reverse:
        cmd.append("-R")

    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=duration + 15)

    return _parse_iperf3_json(proc.stdout)


def _parse_iperf3_json(output: str) -> IperfResult:
    """Parse iperf3 JSON output into an IperfResult."""
    try:
        data = json.loads(output)
    except (json.JSONDecodeError, TypeError):
        return IperfResult()

    result = IperfResult(raw=data)
    end = data.get("end", {})

    # TCP results
    sum_sent = end.get("sum_sent", {})
    if sum_sent:
        result.bits_per_second = sum_sent.get("bits_per_second", 0.0)
        result.retransmits = sum_sent.get("retransmits", 0)
        return result

    # UDP results
    sum_data = end.get("sum", {})
    if sum_data:
        result.bits_per_second = sum_data.get("bits_per_second", 0.0)
        result.lost_percent = sum_data.get("lost_percent", 0.0)
        result.jitter_ms = sum_data.get("jitter_ms", 0.0)
        return result

    # Parallel TCP: look at sum_sent in streams summary
    streams = end.get("streams", [])
    if streams:
        total_bps = sum(s.get("sender", {}).get("bits_per_second", 0.0) for s in streams)
        total_retransmits = sum(s.get("sender", {}).get("retransmits", 0) for s in streams)
        result.bits_per_second = total_bps
        result.retransmits = total_retransmits

    return result
