"""Shared VM infrastructure for wallhack benchmark and test runners."""

import collections, json, os, pathlib, signal, socket, subprocess, sys, threading, time

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
VMLINUZ = REPO_ROOT / "bench" / "vm" / "staging" / "vmlinuz"
INITRD = REPO_ROOT / "bench" / "vm" / "staging" / "initrd.gz"
LOG_DIR = REPO_ROOT / "bench" / "results" / "logs"
RESULTS_DIR = REPO_ROOT / "bench" / "results"
QEMU = "qemu-system-x86_64"
ENTRY_READY_TIMEOUT = 30
RESULT_TIMEOUT = 90
RING_BUFFER_SIZE = 500


# ── preflight ─────────────────────────────────────────────────────────────────


def preflight():
    errs = []
    if not os.access("/dev/kvm", os.R_OK | os.W_OK):
        errs.append("/dev/kvm not accessible")
    if subprocess.run(["which", QEMU], capture_output=True).returncode != 0:
        errs.append(f"{QEMU} not found")
    if not VMLINUZ.exists():
        errs.append(f"kernel not found: {VMLINUZ}")
    if not INITRD.exists():
        errs.append(f"initramfs not found: {INITRD}")
    for e in errs:
        print(f"Error: {e}", file=sys.stderr)
    if errs:
        sys.exit(1)


# ── QEMU helpers ──────────────────────────────────────────────────────────────


def qemu_base(append, extra=None):
    """Common QEMU args: microvm machine, KVM, 256M, kernel+initrd, nographic.

    append -- kernel command line string (passed to QEMU's -append flag)
    extra  -- additional QEMU flags inserted before -append, e.g. -netdev/-device pairs
    """
    return [
        QEMU,
        "-M",
        "microvm,acpi=off,pit=off,pic=off,rtc=off",
        "-enable-kvm",
        "-cpu",
        "host",
        "-m",
        "256M",
        "-smp",
        "2",
        "-kernel",
        str(VMLINUZ),
        "-initrd",
        str(INITRD),
        "-nographic",
        "-no-reboot",
        *(extra or []),
        "-append",
        append,
    ]


def qemu_cmd(port, role, scenario, transport, netem=None, metric=None, debug=False):
    """Build a QEMU command for a wallhack VM.

    port   — TCP port for inter-VM L2 socket (entry listens, exit connects)
    netem  — dict with optional 'loss', 'delay', and 'rate' keys (test runner)
    metric — benchmark metric name string (benchmark runner)
    """
    # Unique MAC for each role to avoid L2 conflicts
    mac = "52:54:00:12:34:56" if role == "exit" else "52:54:00:12:34:57"

    # Entry VM binds the listen socket; exit VM connects to it.
    # TCP listen/connect avoids the AF_UNIX socketpair QEMU assertion
    # (net_fill_rstate: size == 0) that fires under high-throughput iperf3 load.
    if role == "entry":
        netdev = f"socket,id=net0,listen=127.0.0.1:{port}"
    else:
        netdev = f"socket,id=net0,connect=127.0.0.1:{port}"

    # ipv6.disable=1: kernel has IPv6 compiled in (olddefconfig default y)
    # but we disable it at boot to exercise the IPv4 fallback path —
    # ipv6_supported() returns false, parse_listen_addr picks 0.0.0.0.
    cmdline = (
        f"console=ttyS0 quiet loglevel=0 net.ifnames=0 biosdevname=0 ipv6.disable=1 rdinit=/init"
        f" wallhack.role={role} wallhack.scenario={scenario}"
        f" wallhack.transport={transport}"
    )
    if metric:
        cmdline += f" wallhack.metric={metric}"
    if netem:
        if "loss" in netem:
            cmdline += f" wallhack.loss={netem['loss']}"
        if "delay" in netem:
            cmdline += f" wallhack.delay={netem['delay']}"
        if "rate" in netem:
            cmdline += f" wallhack.rate={netem['rate']}"
    if debug:
        cmdline += " wallhack.debug=1"

    return qemu_base(
        cmdline,
        extra=[
            "-netdev",
            netdev,
            "-device",
            f"virtio-net-device,netdev=net0,mac={mac}",
        ],
    )


def start_vm(cmd):
    return subprocess.Popen(
        cmd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )


def kill_vm(proc):
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    except (OSError, ProcessLookupError):
        pass
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (OSError, ProcessLookupError):
            pass


# ── concurrent log drain ──────────────────────────────────────────────────────
# Each QEMU stdout is drained by a daemon thread into a shared deque.
# The main thread polls the deque — no synchronous cross-process blocking.


def drain(proc, log):
    """Background daemon: drain proc.stdout into log (deque of str)."""
    for raw in iter(proc.stdout.readline, b""):
        log.append(raw.decode(errors="replace").rstrip())


def wait_for_token(log, proc, token, timeout):
    """Poll log for token. Returns (matched_line, error_str)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        # Scan the full snapshot each iteration: the bounded deque evicts old
        # entries from the front when full, making index-based seen_count wrong.
        for line in list(log):
            if token in line:
                return line, None

        if proc.poll() is not None:
            return None, f"process exited (rc={proc.returncode}) before {token!r}"
        time.sleep(0.05)
    return None, f"timeout ({timeout}s) waiting for {token!r}"


def wait_for_result(log, proc, timeout):
    """Poll log for WALLHACK_RESULT_MAGIC_TOKEN. Returns (dict, error_str)."""
    prefix = "WALLHACK_RESULT_MAGIC_TOKEN: "
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        # Scan the full snapshot each iteration: the bounded deque evicts old
        # entries from the front when full, making index-based seen_count wrong.
        for line in list(log):
            if line.startswith(prefix):
                try:
                    return json.loads(line[len(prefix) :]), None
                except json.JSONDecodeError as e:
                    return None, f"bad result JSON: {e}"

        if proc.poll() is not None and proc.returncode != 0:
            return None, f"process exited (rc={proc.returncode}) before result token"
        time.sleep(0.05)
    return None, f"timeout ({timeout}s) waiting for WALLHACK_RESULT_MAGIC_TOKEN"


def make_log_pair():
    """Return a fresh (exit_log, entry_log) deque pair."""
    return (
        collections.deque(maxlen=RING_BUFFER_SIZE),
        collections.deque(maxlen=RING_BUFFER_SIZE),
    )


def free_port():
    """Allocate a free ephemeral TCP port for inter-VM QEMU socket networking."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]
