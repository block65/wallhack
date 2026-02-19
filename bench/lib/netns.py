"""Network namespace and veth pair management via subprocess + ip commands."""

from __future__ import annotations

import subprocess


def run(cmd: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        shell=True,
        check=check,
        capture_output=True,
        text=True,
    )


def ns_exec(ns: str, cmd: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    if ns:
        return run(f"ip netns exec {ns} {cmd}", check=check)
    return run(cmd, check=check)


def create_namespace(name: str) -> None:
    run(f"ip netns add {name}")


def delete_namespace(name: str) -> None:
    run(f"ip netns del {name}", check=False)


def create_veth_pair(
    ns_a: str,
    veth_a: str,
    ip_a: str,
    prefix_len: int,
    ns_b: str,
    veth_b: str,
    ip_b: str,
) -> None:
    """Create a veth pair and move each end into its namespace with IP."""
    run(f"ip link add {veth_a} type veth peer name {veth_b}")
    run(f"ip link set {veth_a} netns {ns_a}")
    run(f"ip link set {veth_b} netns {ns_b}")
    ns_exec(ns_a, f"ip addr add {ip_a}/{prefix_len} dev {veth_a}")
    ns_exec(ns_a, f"ip link set {veth_a} up")
    ns_exec(ns_a, f"ip link set lo up")
    ns_exec(ns_b, f"ip addr add {ip_b}/{prefix_len} dev {veth_b}")
    ns_exec(ns_b, f"ip link set {veth_b} up")
    ns_exec(ns_b, f"ip link set lo up")


def add_route(ns: str, dest: str, dev: str) -> None:
    ns_exec(ns, f"ip route replace {dest} dev {dev}")


def link_exists(ns: str, name: str) -> bool:
    result = ns_exec(ns, f"ip link show {name}", check=False)
    return result.returncode == 0


def link_is_up(ns: str, name: str) -> bool:
    result = ns_exec(ns, f"ip link show {name} up", check=False)
    return result.returncode == 0 and name in result.stdout


def set_link_up(ns: str, name: str) -> None:
    ns_exec(ns, f"ip link set {name} up")


def create_tun(ns: str, name: str) -> None:
    """Pre-create a TUN device and bring it up inside a namespace."""
    ns_exec(ns, f"ip tuntap add dev {name} mode tun")
    ns_exec(ns, f"ip link set {name} up")


def set_netem(ns: str, dev: str, loss_pct: float, delay_ms: float) -> None:
    """Apply (or atomically replace) a netem qdisc on an interface.

    Uses 'replace' so it works whether or not a qdisc already exists.
    """
    parts = []
    if delay_ms > 0:
        parts.append(f"delay {delay_ms}ms")
    if loss_pct > 0:
        parts.append(f"loss {loss_pct}%")
    if parts:
        ns_exec(ns, f"tc qdisc replace dev {dev} root netem {' '.join(parts)}")


def clear_netem(ns: str, dev: str) -> None:
    """Remove the root qdisc from an interface (no-op if none exists)."""
    ns_exec(ns, f"tc qdisc del dev {dev} root", check=False)


def destroy_namespaces(*names: str) -> None:
    for name in names:
        delete_namespace(name)
