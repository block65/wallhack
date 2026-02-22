#!/usr/bin/env python3
"""wallhack benchmark runner.

Usage:
    python3 bench/run_benchmarks.py [--transport quic|websocket|both] [--runs N] [--debug] [--verbose]
"""
import argparse, collections, json, os, pathlib, signal, socket, statistics, subprocess, sys, threading, time

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
VMLINUZ = REPO_ROOT / "bench" / "vm" / "staging" / "vmlinuz"
INITRD = REPO_ROOT / "bench" / "vm" / "staging" / "initrd.gz"
LOG_DIR = REPO_ROOT / "bench" / "results" / "logs"
RESULTS_DIR = REPO_ROOT / "bench" / "results"
QEMU = "qemu-system-x86_64"
EXIT_READY_TIMEOUT = 60
RESULT_TIMEOUT = 120
RING_BUFFER_SIZE = 500

SCENARIOS = [
    ("benchmark", "tcp_fwd", "throughput_mbps"),
    ("benchmark", "tcp_rev", "throughput_mbps"),
    ("benchmark", "udp", "throughput_mbps"),
    ("benchmark", "latency", "latency_ms"),
    ("benchmark", "parallel2", "throughput_mbps"),
    ("benchmark", "parallel4", "throughput_mbps"),
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


def qemu_cmd(fd, role, scenario, transport, metric, debug=False):
    # Unique MAC for each role to avoid L2 conflicts
    mac = "52:54:00:12:34:56" if role == "exit" else "52:54:00:12:34:57"

    cmdline = (
        f"console=ttyS0 quiet loglevel=0 net.ifnames=0 biosdevname=0 rdinit=/init"
        f" wallhack.role={role} wallhack.scenario={scenario}"
        f" wallhack.transport={transport} wallhack.metric={metric}"
    )
    if debug:
        cmdline += " wallhack.debug=1"

    parts = [
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
        "-netdev",
        f"socket,id=net0,fd={fd}",
        "-device",
        f"virtio-net-device,netdev=net0,mac={mac}",
        "-nographic",
        "-no-reboot",
        "-append",
        cmdline,
    ]
    return parts


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


def _drain(proc, log):
    for raw in iter(proc.stdout.readline, b""):
        log.append(raw.decode(errors="replace").rstrip())


def _wait_for_token(log, proc, token, timeout):
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


def run_one_benchmark(transport, metric, debug=False):
    sock_a, sock_b = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    exit_log = collections.deque(maxlen=RING_BUFFER_SIZE)
    entry_log = collections.deque(maxlen=RING_BUFFER_SIZE)
    entry_proc = None

    exit_proc = start_vm(
        qemu_cmd(sock_a.fileno(), "exit", "benchmark", transport, metric, debug),
        sock_a.fileno(),
    )
    sock_a.close()

    exit_drainer = threading.Thread(target=_drain, args=(exit_proc, exit_log), daemon=True)
    exit_drainer.start()

    try:
        _, err = _wait_for_token(
            exit_log, exit_proc, "WALLHACK_EXIT_READY_MAGIC_TOKEN", EXIT_READY_TIMEOUT
        )
        if err:
            sock_b.close()
            return None, f"exit VM: {err}", exit_log, entry_log

        entry_proc = start_vm(
            qemu_cmd(sock_b.fileno(), "entry", "benchmark", transport, metric, debug),
            sock_b.fileno(),
        )
        sock_b.close()

        entry_drainer = threading.Thread(
            target=_drain, args=(entry_proc, entry_log), daemon=True
        )
        entry_drainer.start()

        outcome, err = _wait_for_result(entry_log, entry_proc, RESULT_TIMEOUT)
        if err:
            return None, f"entry VM: {err}", exit_log, entry_log
        
        if outcome and outcome.get("status") == "pass":
            val = outcome.get("value_mbps") if "value_mbps" in outcome else outcome.get("value_ms")
            return float(val), "", exit_log, entry_log
        
        return None, outcome.get("reason", "unknown"), exit_log, entry_log
    finally:
        if entry_proc:
            kill_vm(entry_proc)
        kill_vm(exit_proc)


# ── wallhack version ──────────────────────────────────────────────────────────


def _wallhack_version():
    wh = REPO_ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "wallhack"
    if not wh.exists():
        wh = REPO_ROOT / "target" / "release" / "wallhack"
    if not wh.exists():
        return "unknown"
    r = subprocess.run([str(wh), "--version"], capture_output=True, text=True)
    return r.stdout.strip().split()[-1] if r.returncode == 0 else "unknown"


# ── main ──────────────────────────────────────────────────────────────────────


ap = argparse.ArgumentParser(description="wallhack benchmark runner")
ap.add_argument("--transport", choices=["quic", "websocket", "both"], default="both")
ap.add_argument("--runs", type=int, default=3)
ap.add_argument("--debug", action="store_true")
ap.add_argument("--verbose", action="store_true")
args = ap.parse_args()

preflight()
signal.signal(signal.SIGINT, lambda _s, _f: sys.exit(130))

transports = ["quic", "websocket"] if args.transport == "both" else [args.transport]
results = {
    "version": 1,
    "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "wallhack_version": _wallhack_version(),
    "scenarios": []
}

for transport in transports:
    for _, metric_name, unit in SCENARIOS:
        label = f"{transport}/{metric_name}"
        print(f"Benchmarking {label} ({args.runs} runs)...", end=" ", flush=True)
        
        run_values = []
        last_err = ""
        last_exit_log, last_entry_log = None, None
        for i in range(args.runs):
            val, err, exit_log, entry_log = run_one_benchmark(transport, metric_name, args.debug)
            if val is not None:
                run_values.append(val)
            else:
                last_err = err
                last_exit_log = exit_log
                last_entry_log = entry_log
        
        if run_values:
            run_values.sort()
            min_val = run_values[0]
            max_val = run_values[-1]
            median = statistics.median(run_values)
            print(f"median: {median:.2f} {unit}")
            
            results["scenarios"].append({
                "name": metric_name,
                "transport": transport,
                "metric": unit,
                "runs": run_values,
                "min": min_val,
                "median": median,
                "max": max_val
            })
        else:
            print(f"FAILED: {last_err}")
            if args.verbose or args.debug:
                log_lines = 500 if args.debug else 50
                if last_exit_log:
                    print(f"  --- exit VM (last {log_lines} lines) ---")
                    print("\n".join(list(last_exit_log)[-log_lines:]))
                if last_entry_log:
                    print(f"  --- entry VM (last {log_lines} lines) ---")
                    print("\n".join(list(last_entry_log)[-log_lines:]))

# Write results
RESULTS_DIR.mkdir(parents=True, exist_ok=True)
ts = time.strftime("%Y%m%d-%H%M%S")
res_file = RESULTS_DIR / f"bench-{ts}.json"
res_file.write_text(json.dumps(results, indent=2))
latest = RESULTS_DIR / "latest"
if latest.is_symlink() or latest.exists():
    latest.unlink()
latest.symlink_to(res_file.name)
print(f"\nResults written to {res_file}")
