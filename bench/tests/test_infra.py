"""Infrastructure tests: verify netns + veth connectivity without wallhack."""

from __future__ import annotations

import subprocess
import textwrap

import pytest

from lib.constants import (
    ECHO_PORT,
    IP_CLIENT,
    IP_ENTRY_CLIENT_SIDE,
    IP_ENTRY_EXIT_SIDE,
    IP_EXIT_ENTRY_SIDE,
    IP_EXIT_TARGET_SIDE,
    IP_TARGET,
    NS_CLIENT,
    NS_ENTRY,
    NS_EXIT,
    NS_TARGET,
)
from lib.netns import ns_exec

pytestmark = pytest.mark.infra


# ---------------------------------------------------------------------------
# Ping reachability over veth pairs
# ---------------------------------------------------------------------------


def test_client_pings_entry(netns_topology: None) -> None:
    """wh-client can reach wh-entry over the client-entry veth pair."""
    result = ns_exec(NS_CLIENT, f"ping -c 1 -W 2 {IP_ENTRY_CLIENT_SIDE}", check=False)
    assert result.returncode == 0, f"ping failed: {result.stderr}"


def test_entry_pings_client(netns_topology: None) -> None:
    """wh-entry can reach wh-client over the client-entry veth pair."""
    result = ns_exec(NS_ENTRY, f"ping -c 1 -W 2 {IP_CLIENT}", check=False)
    assert result.returncode == 0, f"ping failed: {result.stderr}"


def test_entry_pings_exit(netns_topology: None) -> None:
    """wh-entry can reach wh-exit over the entry-exit veth pair."""
    result = ns_exec(NS_ENTRY, f"ping -c 1 -W 2 {IP_EXIT_ENTRY_SIDE}", check=False)
    assert result.returncode == 0, f"ping failed: {result.stderr}"


def test_exit_pings_entry(netns_topology: None) -> None:
    """wh-exit can reach wh-entry over the entry-exit veth pair."""
    result = ns_exec(NS_EXIT, f"ping -c 1 -W 2 {IP_ENTRY_EXIT_SIDE}", check=False)
    assert result.returncode == 0, f"ping failed: {result.stderr}"


def test_exit_pings_target(netns_topology: None) -> None:
    """wh-exit can reach wh-target over the exit-target veth pair."""
    result = ns_exec(NS_EXIT, f"ping -c 1 -W 2 {IP_TARGET}", check=False)
    assert result.returncode == 0, f"ping failed: {result.stderr}"


def test_target_pings_exit(netns_topology: None) -> None:
    """wh-target can reach wh-exit over the exit-target veth pair."""
    result = ns_exec(NS_TARGET, f"ping -c 1 -W 2 {IP_EXIT_TARGET_SIDE}", check=False)
    assert result.returncode == 0, f"ping failed: {result.stderr}"


# ---------------------------------------------------------------------------
# Echo server reachable directly from exit namespace
# ---------------------------------------------------------------------------


def test_echo_from_exit(netns_topology: None) -> None:
    """wh-exit can TCP echo to the target (no tunnel involved)."""
    script = textwrap.dedent(f"""\
        import socket
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(("{IP_TARGET}", {ECHO_PORT}))
        s.sendall(b"infra-test")
        data = s.recv(1024)
        s.close()
        assert data == b"infra-test", f"got {{data!r}}"
        print("OK", end="")
    """)
    result = subprocess.run(
        ["ip", "netns", "exec", NS_EXIT, "python3", "-c", script],
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, f"echo failed: {result.stderr}"
    assert result.stdout == "OK"


# ---------------------------------------------------------------------------
# IP forwarding enabled in entry and exit namespaces
# ---------------------------------------------------------------------------


def test_entry_ip_forward(netns_topology: None) -> None:
    """wh-entry has IP forwarding enabled."""
    result = ns_exec(NS_ENTRY, "sysctl -n net.ipv4.ip_forward", check=False)
    assert result.stdout.strip() == "1", "ip_forward not enabled in wh-entry"


def test_exit_ip_forward(netns_topology: None) -> None:
    """wh-exit has IP forwarding enabled."""
    result = ns_exec(NS_EXIT, "sysctl -n net.ipv4.ip_forward", check=False)
    assert result.stdout.strip() == "1", "ip_forward not enabled in wh-exit"
