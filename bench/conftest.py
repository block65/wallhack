"""Root conftest: fixtures, CLI options, markers for wallhack benchmarks."""

from __future__ import annotations

import os
import subprocess
import time
from pathlib import Path

import pytest

from lib.constants import (
    EXIT_ID,
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
    PREFIX_LEN,
    PROCESS_STARTUP_DELAY,
    SUBNET_CLIENT_ENTRY,
    SUBNET_EXIT_TARGET,
    TUN_NAME,
    VETH_CE_CLIENT,
    VETH_CE_ENTRY,
    VETH_EE_ENTRY,
    VETH_EE_EXIT,
    VETH_ET_EXIT,
    VETH_ET_TARGET,
    WALLHACK_LISTEN_PORT,
)
from lib.echo_server import EchoServer
from lib.iperf import Iperf3Server
from lib.netns import (
    add_route,
    create_namespace,
    create_veth_pair,
    destroy_namespaces,
    ns_exec,
)
from lib.wallhack import WallhackProcess

ROOT = Path(__file__).resolve().parent.parent


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--wallhack-bin",
        default=str(ROOT / "target" / "release" / "wallhack"),
        help="Path to wallhack binary",
    )
    parser.addoption(
        "--iperf3-bin",
        default=str(Path(__file__).resolve().parent / "bin" / "iperf3"),
        help="Path to iperf3 binary",
    )


def pytest_configure(config: pytest.Config) -> None:
    if os.geteuid() != 0:
        pytest.exit("bench tests require root (use sudo)", returncode=1)
    # Kill any stale wallhack processes from previous runs
    subprocess.run(
        ["pkill", "-9", "-f", "wallhack.*(-l|-c)"],
        capture_output=True,
        check=False,
    )
    # Clean up any stale namespaces
    destroy_namespaces(NS_CLIENT, NS_ENTRY, NS_EXIT, NS_TARGET)


@pytest.fixture(scope="session")
def wallhack_bin(request: pytest.FixtureRequest) -> str:
    path = request.config.getoption("--wallhack-bin")
    if not Path(path).is_file():
        pytest.skip(f"wallhack binary not found: {path}")
    return path


@pytest.fixture(scope="session")
def iperf3_bin(request: pytest.FixtureRequest) -> str:
    path = request.config.getoption("--iperf3-bin")
    if not Path(path).is_file():
        pytest.skip(f"iperf3 binary not found: {path}")
    return path


@pytest.fixture(scope="session")
def netns_topology():
    """Create 4 namespaces, 3 veth pairs, enable forwarding, start echo server.

    Topology (matches diag.sh):
      wh-client (10.200.0.10) <--veth--> wh-entry (10.200.0.1 + TUN)
      wh-entry (10.200.1.10) <--veth--> wh-exit (10.200.1.20)
      wh-exit (10.200.2.20) <--veth--> wh-target (10.200.2.10)

    Traffic flow: client -> entry TUN -> smoltcp -> QUIC -> exit -> target
    """
    echo = None

    try:
        # Clean up any leftover namespaces
        destroy_namespaces(NS_CLIENT, NS_ENTRY, NS_EXIT, NS_TARGET)

        # Create namespaces
        create_namespace(NS_CLIENT)
        create_namespace(NS_ENTRY)
        create_namespace(NS_EXIT)
        create_namespace(NS_TARGET)

        # client <-> entry veth
        create_veth_pair(
            NS_CLIENT, VETH_CE_CLIENT, IP_CLIENT, PREFIX_LEN,
            NS_ENTRY, VETH_CE_ENTRY, IP_ENTRY_CLIENT_SIDE,
        )

        # entry <-> exit veth
        create_veth_pair(
            NS_ENTRY, VETH_EE_ENTRY, IP_ENTRY_EXIT_SIDE, PREFIX_LEN,
            NS_EXIT, VETH_EE_EXIT, IP_EXIT_ENTRY_SIDE,
        )

        # exit <-> target veth
        create_veth_pair(
            NS_EXIT, VETH_ET_EXIT, IP_EXIT_TARGET_SIDE, PREFIX_LEN,
            NS_TARGET, VETH_ET_TARGET, IP_TARGET,
        )

        # Enable IP forwarding in entry namespace (for TUN traffic)
        ns_exec(NS_ENTRY, "sysctl -w net.ipv4.ip_forward=1")

        # Enable IP forwarding in exit namespace (it routes between subnets)
        ns_exec(NS_EXIT, "sysctl -w net.ipv4.ip_forward=1")

        # Start echo server in target namespace
        echo = EchoServer(NS_TARGET)
        echo.start()
        time.sleep(PROCESS_STARTUP_DELAY)

        # Verify echo server is reachable from exit namespace
        verify_script = f'''
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(2)
s.connect(("{IP_TARGET}", {ECHO_PORT}))
s.sendall(b"test")
data = s.recv(4)
s.close()
assert data == b"test", f"Echo mismatch: {{data}}"
print("OK")
'''
        try:
            ns_exec(NS_EXIT, f"python3 -c '{verify_script}'")
        except subprocess.CalledProcessError:
            raise RuntimeError(f"Echo server not reachable from {NS_EXIT} at {IP_TARGET}:{ECHO_PORT}")

        yield

    finally:
        if echo:
            echo.stop()
        destroy_namespaces(NS_CLIENT, NS_ENTRY, NS_EXIT, NS_TARGET)


class TopologyState:
    """Holds references to running wallhack processes for diagnostics."""

    def __init__(self, entry: WallhackProcess, exit_node: WallhackProcess) -> None:
        self.entry = entry
        self.exit_node = exit_node

    def dump(self) -> str:
        lines = []
        lines.append(f"--- entry (pid={self.entry.pid}) ---")
        lines.append(self.entry.output() or "(no output)")
        lines.append(f"--- exit (pid={self.exit_node.pid}) ---")
        lines.append(self.exit_node.output() or "(no output)")

        for label, ns_name in [("client", NS_CLIENT), ("entry", NS_ENTRY), ("exit", NS_EXIT), ("target", NS_TARGET)]:
            r = ns_exec(ns_name, "ip addr show", check=False)
            lines.append(f"--- {label} ({ns_name}) interfaces ---")
            lines.append(r.stdout or "(empty)")
            r = ns_exec(ns_name, "ip route show", check=False)
            lines.append(f"--- {label} ({ns_name}) routes ---")
            lines.append(r.stdout or "(empty)")

        return "\n".join(lines)


@pytest.fixture(scope="session")
def topology(netns_topology: None, wallhack_bin: str) -> TopologyState:
    """Start wallhack entry + exit on top of the netns topology."""
    entry_proc = None
    exit_proc = None

    try:
        # Start wallhack entry node (server mode, listens for exit connections)
        entry_proc = WallhackProcess(
            ns=NS_ENTRY,
            args=["-l", f":{WALLHACK_LISTEN_PORT}", "--debug"],
            binary=wallhack_bin,
            env={
                "RUST_LOG": os.environ.get("RUST_LOG", "wallhack=info,netstack=info"),
                "NO_COLOR": "1",
            },
        )
        entry_proc.start(log_file="/tmp/wallhack-entry.log")
        time.sleep(PROCESS_STARTUP_DELAY)

        # Start wallhack exit node (client mode, connects to entry)
        exit_proc = WallhackProcess(
            ns=NS_EXIT,
            args=[
                "-c", f"{IP_ENTRY_EXIT_SIDE}:{WALLHACK_LISTEN_PORT}",
                "-i", EXIT_ID,
                "--debug",
            ],
            binary=wallhack_bin,
        )
        exit_proc.start(log_file="/tmp/wallhack-exit.log")

        # Wait for TUN interface to come UP in entry namespace
        # (wallhack creates tun-bench when the exit node connects)
        entry_proc.wait_for_tun(ns=NS_ENTRY)

        # Add route for target subnet via TUN in entry namespace
        add_route(NS_ENTRY, SUBNET_EXIT_TARGET, TUN_NAME)

        # Add default route via entry in client namespace (traffic goes through TUN)
        ns_exec(NS_CLIENT, f"ip route add {SUBNET_EXIT_TARGET} via {IP_ENTRY_CLIENT_SIDE}")

        state = TopologyState(entry_proc, exit_proc)

        # Dump topology state for diagnostics
        print(f"\n{state.dump()}")

        yield state

    finally:
        if exit_proc:
            exit_proc.stop()
        if entry_proc:
            entry_proc.stop()


@pytest.fixture
def iperf3_server(topology: None, iperf3_bin: str) -> Iperf3Server:
    """Start an iperf3 server in the target namespace for one test."""
    server = Iperf3Server(NS_TARGET, binary=iperf3_bin)
    server.start()
    time.sleep(PROCESS_STARTUP_DELAY)
    try:
        yield server
    finally:
        server.stop()
        time.sleep(0.5)  # Allow TIME_WAIT sockets to clear
