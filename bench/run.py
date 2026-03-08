#!/usr/bin/env python3
"""wallhack unified test & benchmark runner.

All scenarios are the same thing: boot two VMs, run iperf3, get a number.
Tags (smoke, resilience, benchmark) are just filters on a flat scenario list.
Execution parameters (runs, duration, concurrency) come from the CLI.

Usage:
    python3 bench/run.py                                   # all scenarios
    python3 bench/run.py --tag smoke                      # filter by tag
    python3 bench/run.py --tag smoke,resilience            # multiple tags
    python3 bench/run.py --tag benchmark --runs 3          # benchmark with repeats
    python3 bench/run.py --metric parallel32 --transport quic --json
    python3 bench/run.py debug-topology [--transport quic|websocket]
    python3 bench/run.py debug-shell
"""
import argparse, concurrent.futures, json, os, signal, statistics, subprocess, sys, threading, time

from vm_common import (
    LOG_DIR,
    REPO_ROOT,
    ENTRY_READY_TIMEOUT,
    RESULT_TIMEOUT,
    RESULTS_DIR,
    preflight,
    qemu_cmd,
    qemu_base,
    start_vm,
    kill_vm,
    drain,
    wait_for_token,
    wait_for_result,
    make_log_pair,
    free_port,
)


# ── scenario config ──────────────────────────────────────────────────────────
# A scenario is just what to measure: metric + transport + netem + threshold.
# How to measure (runs, duration, concurrency) comes from the CLI.
#
# Fields:
#   tags      — set of filter tags (e.g. {"smoke"}, {"benchmark"})
#   metric    — iperf3 metric name (parallel4, tcp_upstream, latency, etc.)
#   unit      — result unit ("throughput_mbps" or "latency_ms")
#   transport — "quic", "websocket", or None (= expand to both)
#   netem     — optional {loss, delay, rate} dict
#   threshold — optional min value; below = fail


def S(tags, metric, unit="throughput_mbps", transport=None, netem=None, threshold=None):
    if isinstance(tags, str):
        tags = {tags}
    s = {"tags": tags, "metric": metric, "unit": unit}
    if transport:
        s["transport"] = transport
    if netem:
        s["netem"] = netem
    if threshold is not None:
        s["threshold"] = threshold
    return s


SCENARIOS = [
    # ── Smoke — quick connectivity proof ─────────────────────────────────────
    S("smoke", "parallel4", transport="quic",      threshold=1),
    S("smoke", "parallel4", transport="websocket",  threshold=1),

    # ── Resilience — connectivity under degraded network ─────────────────────
    S("resilience", "parallel4", transport="quic",      threshold=1, netem={"loss": "0.5%", "delay": "5ms"}),
    S("resilience", "parallel4", transport="quic",      threshold=1, netem={"loss": "2%", "delay": "25ms"}),
    S("resilience", "parallel4", transport="quic",      threshold=1, netem={"loss": "0%", "delay": "100ms"}),
    S("resilience", "parallel4", transport="websocket", threshold=1, netem={"loss": "0.5%", "delay": "5ms"}),
    S("resilience", "parallel4", transport="websocket", threshold=1, netem={"loss": "2%", "delay": "25ms"}),
    S("resilience", "parallel4", transport="websocket", threshold=1, netem={"loss": "0%", "delay": "100ms"}),

    # ── Benchmark — loopback (no netem) ──────────────────────────────────────
    S("benchmark", "tcp_upstream"),
    S("benchmark", "tcp_downstream"),
    S("benchmark", "udp"),
    S("benchmark", "latency", unit="latency_ms"),
    S("benchmark", "parallel4"),
    S("benchmark", "parallel8"),
    S("benchmark", "parallel32"),
    S("benchmark", "parallel64"),
    S("benchmark", "parallel128"),

    # ── Benchmark — loss + delay sweep (40ms one-way / 80ms RTT) ─────────────
    *[S("benchmark", m, netem={"loss": l, "delay": "40ms"})
      for m in ("parallel32", "parallel64", "parallel128")
      for l in ("0.1%", "0.5%", "1%", "2%", "3%", "5%")],

    # ── Benchmark — delay sweep (1% loss) ────────────────────────────────────
    *[S("benchmark", m, netem={"loss": "1%", "delay": d})
      for m in ("parallel32", "parallel64", "parallel128")
      for d in ("1ms", "10ms", "20ms", "40ms", "80ms", "150ms")],

    # ── Benchmark — delay only (no loss) ─────────────────────────────────────
    *[S("benchmark", m, netem={"delay": d})
      for m in ("parallel32", "parallel64", "parallel128")
      for d in ("1ms", "10ms", "40ms", "80ms", "150ms")],
]


# ── helpers ──────────────────────────────────────────────────────────────────


def _make_label(scenario):
    transport = scenario.get("transport", "?")
    label = f"{transport}/{scenario['metric']}"
    netem = scenario.get("netem")
    if netem:
        label += " (" + " ".join(f"{k}={v}" for k, v in netem.items()) + ")"
    return label


def _qemu_debug_shell_cmd():
    return qemu_base(
        "console=ttyS0 loglevel=3 net.ifnames=0 biosdevname=0 ipv6.disable=1 rdinit=/bin/sh panic=-1",
    )


def _wallhack_version():
    wh = REPO_ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "wallhack"
    if not wh.exists():
        wh = REPO_ROOT / "target" / "release" / "wallhack"
    if not wh.exists():
        return "unknown"
    r = subprocess.run([str(wh), "--version"], capture_output=True, text=True)
    return r.stdout.split()[1] if r.returncode == 0 else "unknown"


# ── VM runner ────────────────────────────────────────────────────────────────


def run_vm_pair(scenario_name, transport, netem=None, metric=None, duration=None, debug=False, keep_running=False):
    """Boot an entry+exit VM pair and wait for a result.

    Returns (ok, value, reason, duration_s, exit_log, entry_log).
    value is a float metric (None on failure), reason is error string.
    """
    port = free_port()
    exit_log, entry_log = make_log_pair()
    exit_proc = None

    entry_proc = start_vm(
        qemu_cmd(port, "entry", scenario_name, transport, netem,
                 metric=metric, duration=duration, debug=debug)
    )
    entry_drainer = threading.Thread(
        target=drain, args=(entry_proc, entry_log), daemon=True
    )
    entry_drainer.start()

    try:
        _, err = wait_for_token(
            entry_log, entry_proc, "WALLHACK_ENTRY_READY_MAGIC_TOKEN", ENTRY_READY_TIMEOUT,
        )
        if err:
            return False, None, f"entry VM: {err}", 0.0, exit_log, entry_log

        exit_proc = start_vm(
            qemu_cmd(port, "exit", scenario_name, transport, netem,
                     metric=metric, duration=duration, debug=debug)
        )
        exit_drainer = threading.Thread(
            target=drain, args=(exit_proc, exit_log), daemon=True
        )
        exit_drainer.start()

        if keep_running:
            print("[debug-topology] Both VMs running. Ctrl-C to stop.")
            try:
                while entry_proc.poll() is None and exit_proc.poll() is None:
                    time.sleep(0.5)
            except KeyboardInterrupt:
                pass
            return True, None, "", 0.0, [], []

        outcome, err = wait_for_result(entry_log, entry_proc, RESULT_TIMEOUT)
        if err:
            return False, None, f"entry VM: {err}", 0.0, exit_log, entry_log

        if outcome and outcome.get("status") == "pass":
            val = outcome.get("value_mbps") or outcome.get("value_ms")
            dur = float(outcome.get("duration_s", 0))
            return True, float(val) if val is not None else None, "", dur, exit_log, entry_log

        return (
            False, None,
            (outcome or {}).get("reason", "unknown"),
            float((outcome or {}).get("duration_s", 0)),
            exit_log, entry_log,
        )
    finally:
        if exit_proc:
            kill_vm(exit_proc)
        kill_vm(entry_proc)


# ── unified runner ───────────────────────────────────────────────────────────


def run_scenarios(scenarios, concurrency, runs, duration, debug, verbose, json_output):
    """Run a list of scenario dicts. Returns True if all passed."""
    total = len(scenarios)
    completed = [0]  # mutable counter for closure
    print_lock = threading.Lock()

    def _run_one(idx, scenario):
        label = _make_label(scenario)
        transport = scenario["transport"]
        metric = scenario["metric"]
        netem = scenario.get("netem")
        threshold = scenario.get("threshold")

        values = []
        last_reason = ""
        last_exit_log, last_entry_log = None, None

        for run_i in range(runs):
            if not json_output and runs > 1:
                with print_lock:
                    print(f"  [{completed[0]+1}/{total}] {label}  run {run_i+1}/{runs}...", flush=True)

            ok, val, reason, dur, exit_log, entry_log = run_vm_pair(
                "benchmark", transport,
                netem=netem, metric=metric, duration=duration, debug=debug,
            )
            if ok and val is not None:
                values.append(val)
            elif not ok:
                last_reason = reason
                last_exit_log = exit_log
                last_entry_log = entry_log

        # Determine pass/fail
        passed = True
        reason = ""
        if not values:
            passed = False
            reason = last_reason
        elif threshold is not None:
            median = statistics.median(values)
            if median < threshold:
                passed = False
                reason = f"median {median:.2f} below threshold {threshold}"

        return {
            "label": label, "ok": passed, "reason": reason,
            "values": values, "scenario": scenario,
            "exit_log": last_exit_log, "entry_log": last_entry_log,
        }

    def _print_result(r):
        label = r["label"]
        ok = r["ok"]
        values = r["values"]
        unit = r["scenario"].get("unit", "throughput_mbps")

        completed[0] += 1
        progress = f"[{completed[0]}/{total}]"

        if values:
            median = statistics.median(values)
            if len(values) > 1:
                print(f"{progress} {label}  {'PASS' if ok else 'FAIL'}  median: {median:.2f} {unit}  (n={len(values)})", flush=True)
            else:
                print(f"{progress} {label}  {'PASS' if ok else 'FAIL'}  {median:.2f} {unit}", flush=True)
        else:
            print(f"{progress} {label}  FAIL", flush=True)

        if not ok:
            print(f"       reason: {r['reason']}", flush=True)

        show_logs = debug or not ok
        if show_logs:
            log_lines = 500 if (verbose or debug) else 150
            exit_log = r.get("exit_log")
            entry_log = r.get("entry_log")
            if exit_log:
                print(f"  --- exit VM (last {log_lines} lines) ---")
                print("\n".join(f"  {l}" for l in list(exit_log)[-log_lines:]))
            if entry_log:
                print(f"  --- entry VM (last {log_lines} lines) ---")
                print("\n".join(f"  {l}" for l in list(entry_log)[-log_lines:]))

        if not ok:
            ts = time.strftime("%Y%m%d-%H%M%S")
            d = LOG_DIR / f"{ts}-{label.replace('/', '-').replace(' ', '_')}"
            d.mkdir(parents=True, exist_ok=True)
            if r.get("exit_log"):
                (d / "exit.log").write_text("\n".join(r["exit_log"]))
            if r.get("entry_log"):
                (d / "entry.log").write_text("\n".join(r["entry_log"]))
            print(f"       logs: {d}", flush=True)

    # Print header
    if not json_output:
        print(f"Running {total} scenarios (concurrency={concurrency}, runs={runs}, duration={duration}s)", flush=True)

    # Run with specified concurrency, print results as they complete
    passed_count = 0
    json_entries = []

    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
        futures = [pool.submit(_run_one, i, s) for i, s in enumerate(scenarios)]

        for future in concurrent.futures.as_completed(futures):
            r = future.result()
            if r["ok"]:
                passed_count += 1

            if json_output:
                entry = {"name": r["label"], "status": "pass" if r["ok"] else "fail",
                         "unit": r["scenario"].get("unit", "throughput_mbps")}
                if r["values"]:
                    entry["values"] = r["values"]
                    entry["median"] = statistics.median(r["values"])
                    if len(r["values"]) > 1:
                        entry["min"] = min(r["values"])
                        entry["max"] = max(r["values"])
                if not r["ok"]:
                    entry["reason"] = r["reason"]
                if r["scenario"].get("netem"):
                    entry["netem"] = r["scenario"]["netem"]
                json_entries.append(entry)
            else:
                with print_lock:
                    _print_result(r)

    if json_output:
        output = {
            "passed": passed_count,
            "total": total,
            "wallhack_version": _wallhack_version(),
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "results": json_entries,
        }
        print(json.dumps(output, indent=2))
    else:
        status = "ALL PASSED" if passed_count == total else f"{total - passed_count} FAILED"
        print(f"\n{passed_count}/{total} scenarios passed. {status}", flush=True)

    return passed_count == total


# ── scenario expansion ───────────────────────────────────────────────────────


def expand_scenarios(scenarios, transport_filter=None, metric_filter=None):
    """Expand scenarios without explicit transport into per-transport copies, then filter."""
    expanded = []
    for s in scenarios:
        if "transport" in s:
            expanded.append(s)
        else:
            for t in ("quic", "websocket"):
                expanded.append({**s, "transport": t})

    if transport_filter and transport_filter != "both":
        expanded = [s for s in expanded if s["transport"] == transport_filter]
    if metric_filter:
        expanded = [s for s in expanded if s["metric"] == metric_filter]
    return expanded


# ── CLI ──────────────────────────────────────────────────────────────────────


ap = argparse.ArgumentParser(description="wallhack test & benchmark runner")
sub = ap.add_subparsers(dest="cmd")

# Main run command (default when no subcommand given)
run_p = sub.add_parser("run", help="run scenarios (default)")
run_p.add_argument("--tag", type=str, default=None, help="comma-separated tags to filter (e.g. smoke,resilience)")
run_p.add_argument("--transport", choices=["quic", "websocket", "both"], default="both")
run_p.add_argument("--runs", type=int, default=1, help="repetitions per scenario (default 1)")
run_p.add_argument("--duration", type=int, default=5, help="iperf3 test duration in seconds (default 5)")
run_p.add_argument("--concurrency", type=int, default=None, help="max parallel VMs (default: auto)")
run_p.add_argument("--metric", type=str, default=None, help="filter by metric name")
run_p.add_argument("--json", action="store_true", dest="json_output")
run_p.add_argument("--debug", action="store_true")
run_p.add_argument("--verbose", action="store_true")

dt = sub.add_parser("debug-topology")
dt.add_argument("--transport", choices=["quic", "websocket"], default="quic")
sub.add_parser("debug-shell", help="boot a single interactive busybox shell VM")

args = ap.parse_args()
cmd = args.cmd

# Default to "run" when no subcommand given
if cmd is None:
    args = ap.parse_args(["run"])
    cmd = "run"

preflight()
signal.signal(signal.SIGINT, lambda _s, _f: sys.exit(130))

if cmd == "debug-shell":
    shell_cmd = _qemu_debug_shell_cmd()
    print("Booting busybox shell VM — Ctrl-A X to exit.")
    os.execvp(shell_cmd[0], shell_cmd)

if cmd == "debug-topology":
    run_vm_pair("debug-topology", args.transport, debug=True, keep_running=True)
    sys.exit(0)

# Filter scenarios by tags
tag_filter = set(args.tag.split(",")) if args.tag else None
if tag_filter:
    selected = [s for s in SCENARIOS if s["tags"] & tag_filter]
else:
    selected = list(SCENARIOS)

selected = expand_scenarios(selected, args.transport, args.metric)

if not selected:
    print(f"No scenarios matched (tags={args.tag}, transport={args.transport}, metric={args.metric})")
    sys.exit(1)

concurrency = args.concurrency
if concurrency is None:
    concurrency = len(selected)

if not run_scenarios(selected, concurrency, args.runs, args.duration,
                     args.debug, args.verbose, args.json_output):
    sys.exit(1)
