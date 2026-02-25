#!/usr/bin/env python3
"""wallhack benchmark runner.

Usage:
    python3 bench/run_benchmarks.py [--transport quic|websocket|both] [--runs N] [--debug] [--verbose]
"""
import argparse, json, signal, statistics, subprocess, sys, threading, time

from vm_common import (
    REPO_ROOT,
    ENTRY_READY_TIMEOUT,
    RESULT_TIMEOUT,
    RESULTS_DIR,
    preflight,
    qemu_cmd,
    start_vm,
    kill_vm,
    drain,
    wait_for_token,
    wait_for_result,
    make_log_pair,
    free_port,
)

SCENARIOS = [
    ("benchmark", "tcp_fwd", "throughput_mbps", None),
    ("benchmark", "tcp_rev", "throughput_mbps", None),
    ("benchmark", "udp", "throughput_mbps", None),
    ("benchmark", "latency", "latency_ms", None),
    ("benchmark", "parallel2", "throughput_mbps", None),
    ("benchmark", "parallel4", "throughput_mbps", None),
    # Packet-loss throughput (4 parallel streams under netem).
    # delay is one-way, so 5ms delay ≈ 10ms RTT, 25ms ≈ 50ms RTT.
    ("benchmark", "parallel4", "throughput_mbps", {"loss": "0.5%", "delay": "5ms"}),
    ("benchmark", "parallel4", "throughput_mbps", {"loss": "2%", "delay": "25ms"}),
]


# ── scenario runner ───────────────────────────────────────────────────────────


def run_one_benchmark(transport, metric, netem=None, debug=False):
    port = free_port()
    exit_log, entry_log = make_log_pair()
    exit_proc = None

    # Start entry VM first — its QEMU netdev binds the listen socket immediately
    # on startup (before the guest OS even boots), so by the time we get
    # WALLHACK_ENTRY_READY_MAGIC_TOKEN, the port is guaranteed to be bound.
    entry_proc = start_vm(
        qemu_cmd(port, "entry", "benchmark", transport, netem=netem, metric=metric, debug=debug)
    )

    entry_drainer = threading.Thread(
        target=drain, args=(entry_proc, entry_log), daemon=True
    )
    entry_drainer.start()

    try:
        _, err = wait_for_token(
            entry_log,
            entry_proc,
            "WALLHACK_ENTRY_READY_MAGIC_TOKEN",
            ENTRY_READY_TIMEOUT,
        )
        if err:
            return None, f"entry VM: {err}", exit_log, entry_log

        exit_proc = start_vm(
            qemu_cmd(
                port,
                "exit",
                "benchmark",
                transport,
                netem=netem,
                metric=metric,
                debug=debug,
            )
        )

        exit_drainer = threading.Thread(
            target=drain, args=(exit_proc, exit_log), daemon=True
        )
        exit_drainer.start()

        outcome, err = wait_for_result(entry_log, entry_proc, RESULT_TIMEOUT)
        if err:
            return None, f"entry VM: {err}", exit_log, entry_log

        if outcome and outcome.get("status") == "pass":
            val = (
                outcome.get("value_mbps")
                if "value_mbps" in outcome
                else outcome.get("value_ms")
            )
            return float(val), "", exit_log, entry_log

        return None, outcome.get("reason", "unknown"), exit_log, entry_log
    finally:
        if exit_proc:
            kill_vm(exit_proc)
        kill_vm(entry_proc)


# ── wallhack version ──────────────────────────────────────────────────────────


def _wallhack_version():
    wh = REPO_ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "wallhack"
    if not wh.exists():
        wh = REPO_ROOT / "target" / "release" / "wallhack"
    if not wh.exists():
        return "unknown"
    r = subprocess.run([str(wh), "--version"], capture_output=True, text=True)
    # First line is "wallhack <semver>"; take the second word.
    return r.stdout.split()[1] if r.returncode == 0 else "unknown"


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
    "scenarios": [],
}

for transport in transports:
    for _, metric_name, unit, netem in SCENARIOS:
        label = f"{transport}/{metric_name}"
        if netem:
            label += " (" + " ".join(f"{k}={v}" for k, v in netem.items()) + ")"
        print(f"Benchmarking {label} ({args.runs} runs)...", end=" ", flush=True)

        run_values = []
        last_err = ""
        last_exit_log, last_entry_log = None, None
        for i in range(args.runs):
            val, err, exit_log, entry_log = run_one_benchmark(
                transport, metric_name, netem=netem, debug=args.debug
            )
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

            entry = {
                    "name": metric_name,
                    "transport": transport,
                    "metric": unit,
                    "runs": run_values,
                    "min": min_val,
                    "median": median,
                    "max": max_val,
                }
            if netem:
                entry["netem"] = netem
            results["scenarios"].append(entry)
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
