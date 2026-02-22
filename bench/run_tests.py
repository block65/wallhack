#!/usr/bin/env python3
"""wallhack integration test runner — smoke and resilience.

Usage:
    python3 bench/run_tests.py smoke       [--transport quic|websocket|both] [--debug] [--verbose]
    python3 bench/run_tests.py resilience  [--transport quic|websocket|both] [--debug] [--verbose]
    python3 bench/run_tests.py debug-topology [--transport quic|websocket]
"""
import argparse, collections, json, os, pathlib, signal, socket, subprocess, sys, threading, time

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
VMLINUZ = REPO_ROOT / "bench" / "vm" / "staging" / "vmlinuz"
INITRD = REPO_ROOT / "bench" / "vm" / "staging" / "initrd.gz"
LOG_DIR = REPO_ROOT / "bench" / "results" / "logs"
QEMU = "qemu-system-x86_64"
ENTRY_READY_TIMEOUT = 30
RESULT_TIMEOUT = 90
RING_BUFFER_SIZE = 500

SMOKE = [
    ("smoke", "quic", None),
    ("smoke", "websocket", None),
]
RESILIENCE = [
    ("resilience", "quic", {"loss": "0.5%", "delay": "5ms"}),
    ("resilience", "quic", {"loss": "2%", "delay": "25ms"}),
    ("resilience", "quic", {"loss": "0%", "delay": "100ms"}),
    ("resilience", "websocket", {"loss": "0.5%", "delay": "5ms"}),
    ("resilience", "websocket", {"loss": "2%", "delay": "25ms"}),
    ("resilience", "websocket", {"loss": "0%", "delay": "100ms"}),
]

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


def _qemu_base(append, extra=None):
    """Common QEMU args: microvm machine, KVM, 256M, kernel+initrd, nographic."""
    return [
        QEMU,
        "-M", "microvm,acpi=off,pit=off,pic=off,rtc=off",
        "-enable-kvm",
        "-cpu", "host",
        "-m", "256M",
        "-smp", "2",
        "-kernel", str(VMLINUZ),
        "-initrd", str(INITRD),
        "-nographic",
        "-no-reboot",
        *(extra or []),
        "-append", append,
    ]


def qemu_cmd(fd, role, scenario, transport, netem=None, debug=False):
    # Unique MAC for each role to avoid L2 conflicts
    mac = "52:54:00:12:34:56" if role == "exit" else "52:54:00:12:34:57"

    cmdline = (
        f"console=ttyS0 quiet loglevel=0 net.ifnames=0 biosdevname=0 rdinit=/init"
        f" wallhack.role={role} wallhack.scenario={scenario}"
        f" wallhack.transport={transport}"
    )
    if netem:
        if "loss" in netem:
            cmdline += f" wallhack.loss={netem['loss']}"
        if "delay" in netem:
            cmdline += f" wallhack.delay={netem['delay']}"
    if debug:
        cmdline += " wallhack.debug=1"

    return _qemu_base(
        cmdline,
        extra=[
            "-netdev", f"socket,id=net0,fd={fd}",
            "-device", f"virtio-net-device,netdev=net0,mac={mac}",
        ],
    )


def qemu_debug_shell_cmd():
    """Single VM with rdinit=/bin/sh for interactive kernel/OS debugging."""
    return _qemu_base("console=ttyS0 loglevel=3 net.ifnames=0 biosdevname=0 rdinit=/bin/sh")


def start_vm(cmd, fd):
    return subprocess.Popen(
        cmd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        pass_fds=(fd,),
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


def _drain(proc, log):
    """Background daemon: drain proc.stdout into log (deque of str)."""
    for raw in iter(proc.stdout.readline, b""):
        log.append(raw.decode(errors="replace").rstrip())


def _wait_for_token(log, proc, token, timeout):
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


def _wait_for_result(log, proc, timeout):
    """Poll log for WALLHACK_RESULT_MAGIC_TOKEN. Returns (dict, error_str)."""
    prefix = "WALLHACK_RESULT_MAGIC_TOKEN: "
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        # Scan the full snapshot each iteration: the bounded deque evicts old
        # entries from the front when full, making index-based seen_count wrong.
        for line in list(log):
            if line.startswith(prefix):
                try:
                    return json.loads(line[len(prefix):]), None
                except json.JSONDecodeError as e:
                    return None, f"bad result JSON: {e}"

        if proc.poll() is not None and proc.returncode != 0:
            return None, f"process exited (rc={proc.returncode}) before result token"
        time.sleep(0.05)
    return None, f"timeout ({timeout}s) waiting for WALLHACK_RESULT_MAGIC_TOKEN"


# ── scenario runner ───────────────────────────────────────────────────────────


def run_scenario(scenario, transport, netem=None, debug=False, keep_running=False):
    sock_a, sock_b = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    exit_log = collections.deque(maxlen=RING_BUFFER_SIZE)
    entry_log = collections.deque(maxlen=RING_BUFFER_SIZE)
    exit_proc = None

    # Start entry VM first — it's the listener; wallhack entry binds the port
    # and emits WALLHACK_ENTRY_READY_MAGIC_TOKEN before we start the exit VM.
    # This eliminates the guaranteed 50ms–500ms retry delay that occurred when
    # exit started first and hit a "connection refused" on its first attempt.
    entry_proc = start_vm(
        qemu_cmd(sock_b.fileno(), "entry", scenario, transport, netem, debug),
        sock_b.fileno(),
    )
    sock_b.close()

    # Drain entry VM stdout continuously in background — prevents pipe deadlock
    # even when the main thread is not reading from it.
    entry_drainer = threading.Thread(
        target=_drain, args=(entry_proc, entry_log), daemon=True
    )
    entry_drainer.start()

    try:
        _, err = _wait_for_token(
            entry_log, entry_proc, "WALLHACK_ENTRY_READY_MAGIC_TOKEN", ENTRY_READY_TIMEOUT
        )
        if err:
            sock_a.close()
            return False, f"entry VM: {err}", 0.0, exit_log, entry_log

        exit_proc = start_vm(
            qemu_cmd(sock_a.fileno(), "exit", scenario, transport, netem, debug),
            sock_a.fileno(),
        )
        sock_a.close()

        # Drain exit VM stdout continuously in background.
        exit_drainer = threading.Thread(
            target=_drain, args=(exit_proc, exit_log), daemon=True
        )
        exit_drainer.start()

        if keep_running:
            print("[debug-topology] Both VMs running. Ctrl-C to stop.")
            try:
                while entry_proc.poll() is None and exit_proc.poll() is None:
                    time.sleep(0.5)
            except KeyboardInterrupt:
                pass
            return True, "", 0.0, [], []

        outcome, err = _wait_for_result(entry_log, entry_proc, RESULT_TIMEOUT)
        if err:
            return False, f"entry VM: {err}", 0.0, exit_log, entry_log
        if outcome and outcome.get("status") == "pass":
            return True, "", float(outcome.get("duration_s", 0)), exit_log, entry_log
        return (
            False,
            (outcome or {}).get("reason", "unknown"),
            float((outcome or {}).get("duration_s", 0)),
            exit_log,
            entry_log,
        )
    finally:
        if exit_proc:
            kill_vm(exit_proc)
        kill_vm(entry_proc)


# ── main ──────────────────────────────────────────────────────────────────────


ap = argparse.ArgumentParser(description="wallhack integration test runner")
sub = ap.add_subparsers(dest="cmd", required=True)
for name in ("smoke", "resilience"):
    sp = sub.add_parser(name)
    sp.add_argument(
        "--transport", choices=["quic", "websocket", "both"], default="both"
    )
    sp.add_argument("--debug", action="store_true")
    sp.add_argument("--verbose", action="store_true")
dt = sub.add_parser("debug-topology")
dt.add_argument("--transport", choices=["quic", "websocket"], default="quic")
sub.add_parser("debug-shell", help="boot a single interactive busybox shell VM")
args = ap.parse_args()

preflight()
signal.signal(signal.SIGINT, lambda _s, _f: sys.exit(130))

cmd = args.cmd
debug = getattr(args, "debug", False)
verbose = getattr(args, "verbose", False)
transport = getattr(args, "transport", None)

if cmd == "debug-shell":
    # Boot a single VM with rdinit=/bin/sh for interactive kernel/OS debugging.
    # Serial console is attached to the terminal. Use Ctrl-A X to exit QEMU.
    preflight()
    shell_cmd = qemu_debug_shell_cmd()
    print("Booting busybox shell VM — use 'poweroff' or Ctrl-A X to exit.")
    os.execvp(shell_cmd[0], shell_cmd)

if cmd == "debug-topology":
    run_scenario("debug-topology", transport, debug=True, keep_running=True)
    sys.exit(0)

scenarios = {"smoke": SMOKE, "resilience": RESILIENCE}[cmd]
if transport != "both":
    scenarios = [(s, t, n) for s, t, n in scenarios if t == transport]

passed_count = 0
for scenario, t, netem in scenarios:
    label = f"{scenario}/{t}"
    if netem:
        label += " (" + " ".join(f"{k}={v}" for k, v in netem.items()) + ")"
    print(f"Running {label}...", end=" ", flush=True)

    ok, reason, duration, exit_log, entry_log = run_scenario(
        scenario, t, netem=netem, debug=debug
    )
    print(f"[{'PASS' if ok else 'FAIL'}]  ({duration:.1f}s)")

    # Log verbosity levels:
    # (none)   -> nothing on pass; ring buffer tail (150 lines) on fail
    # --verbose -> full ring buffer for failing scenarios only
    # --debug   -> full ring buffer for all scenarios
    show_logs = debug or not ok

    if show_logs:
        log_lines = 500 if (verbose or debug) else 150
        if not ok:
            print(f"       reason: {reason}")

        if exit_log:
            print(f"  --- exit VM (last {log_lines} lines) ---")
            print("\n".join(f"  {l}" for l in list(exit_log)[-log_lines:]))
        if entry_log:
            print(f"  --- entry VM (last {log_lines} lines) ---")
            print("\n".join(f"  {l}" for l in list(entry_log)[-log_lines:]))

    if not ok:
        ts = time.strftime("%Y%m%d-%H%M%S")
        d = LOG_DIR / f"{ts}-{scenario}-{t}"
        d.mkdir(parents=True, exist_ok=True)
        (d / "exit.log").write_text("\n".join(exit_log))
        (d / "entry.log").write_text("\n".join(entry_log))
        print(f"       logs: {d}")
    else:
        passed_count += 1

total = len(scenarios)

print(f"\n{passed_count}/{total} scenarios passed.")

if passed_count < total:
    sys.exit(1)
