#!/usr/bin/env python3
"""wallhack integration test runner — smoke and resilience.

Usage:
    python3 bench/run_tests.py smoke       [--transport quic|websocket|both] [--debug] [--verbose]
    python3 bench/run_tests.py resilience  [--transport quic|websocket|both] [--debug] [--verbose]
    python3 bench/run_tests.py debug-topology [--transport quic|websocket]
"""
import argparse, os, signal, sys, threading, time

from vm_common import (
    LOG_DIR,
    ENTRY_READY_TIMEOUT,
    RESULT_TIMEOUT,
    preflight,
    qemu_cmd,
    qemu_base,
    start_vm,
    kill_vm,
    drain,
    wait_for_token,
    wait_for_result,
    make_log_pair,
    make_socketpair,
)

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


def qemu_debug_shell_cmd():
    """Single VM with rdinit=/bin/sh for interactive kernel/OS debugging."""
    return qemu_base(
        "console=ttyS0 loglevel=3 net.ifnames=0 biosdevname=0 rdinit=/bin/sh panic=-1"
    )


# ── scenario runner ───────────────────────────────────────────────────────────


def run_scenario(scenario, transport, netem=None, debug=False, keep_running=False):
    sock_a, sock_b = make_socketpair()
    exit_log, entry_log = make_log_pair()
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
            sock_a.close()
            return False, f"entry VM: {err}", 0.0, exit_log, entry_log

        exit_proc = start_vm(
            qemu_cmd(sock_a.fileno(), "exit", scenario, transport, netem, debug),
            sock_a.fileno(),
        )
        sock_a.close()

        # Drain exit VM stdout continuously in background.
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
            return True, "", 0.0, [], []

        outcome, err = wait_for_result(entry_log, entry_proc, RESULT_TIMEOUT)
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
