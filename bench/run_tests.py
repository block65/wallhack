#!/usr/bin/env python3
"""wallhack integration test runner — smoke and resilience.

Usage:
    python3 bench/run_tests.py smoke       [--transport quic|websocket|both]
    python3 bench/run_tests.py resilience  [--transport quic|websocket|both]
    python3 bench/run_tests.py debug-topology [--transport quic|websocket]

Flags:
    --verbose   Print full ring buffer for failing scenarios
    --debug     Pass --debug to wallhack; print full ring buffer always
    --transport quic|websocket|both  (default: both for smoke/resilience)
"""

import argparse
import collections
import contextlib
import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import threading
import time

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
VM_IMAGE  = REPO_ROOT / "bench" / "vm" / "base.qcow2"
LOG_DIR   = REPO_ROOT / "bench" / "results" / "logs"

QEMU      = "qemu-system-x86_64"
VM_TIMEOUT_EXIT_READY = 60   # seconds to wait for WALLHACK_EXIT_READY
VM_TIMEOUT_RESULT     = 90   # seconds to wait for WALLHACK_RESULT
RING_BUFFER_SIZE      = 500  # lines

# ── preflight ─────────────────────────────────────────────────────────────────

def preflight():
    errors = []
    if not os.access("/dev/kvm", os.R_OK | os.W_OK):
        errors.append("Error: /dev/kvm not accessible. Add yourself to the kvm group.")
    if subprocess.run(["which", QEMU], capture_output=True).returncode != 0:
        errors.append(f"Error: {QEMU} not found. Install qemu-system-x86.")
    if not VM_IMAGE.exists():
        errors.append("Error: VM image not found. Run: just setup-vm")
    if errors:
        for e in errors:
            print(e, file=sys.stderr)
        sys.exit(1)

# ── VMProcess ─────────────────────────────────────────────────────────────────

class VMProcess:
    """Wraps a QEMU subprocess with a daemon thread that drains stdout
    into a ring buffer.  Safe for single-reader (main thread) access.
    """

    def __init__(self, qemu_args: list[str], label: str, pass_fds=()):
        self.label = label
        self._log: collections.deque[str] = collections.deque(maxlen=RING_BUFFER_SIZE)
        self._proc = subprocess.Popen(
            qemu_args,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            pass_fds=pass_fds,
            preexec_fn=os.setpgrp,
        )
        self._thread = threading.Thread(target=self._drain, daemon=True, name=f"drain-{label}")
        self._thread.start()

    def _drain(self):
        for raw in self._proc.stdout:
            self._log.append(raw.decode(errors="replace").rstrip())

    # ── polling ──────────────────────────────────────────────────────────────

    def wait_for(self, pattern: str, timeout: float) -> tuple[str | None, str | None]:
        """Block until *pattern* appears in the log or timeout/crash.

        Returns (matching_line, None) on success, (None, error_msg) on failure.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            rc = self._proc.poll()
            if rc is not None:
                return None, f"{self.label}: process exited with code {rc}"
            for line in list(self._log):
                if pattern in line:
                    return line, None
            time.sleep(0.1)
        return None, f"{self.label}: timeout ({timeout}s) waiting for {pattern!r}"

    def wait_for_result(self, timeout: float) -> tuple[dict | None, str | None]:
        """Block until a WALLHACK_RESULT: line appears.

        Returns (parsed_dict, None) on success, (None, error_msg) on failure.
        """
        prefix = "WALLHACK_RESULT: "
        deadline = time.monotonic() + timeout
        seen: set[str] = set()
        while time.monotonic() < deadline:
            rc = self._proc.poll()
            if rc is not None and rc != 0:
                return None, f"{self.label}: process exited with code {rc} before result"
            for line in list(self._log):
                if line.startswith(prefix) and line not in seen:
                    seen.add(line)
                    payload = line[len(prefix):]
                    try:
                        return json.loads(payload), None
                    except json.JSONDecodeError as exc:
                        return None, f"{self.label}: malformed result JSON: {exc}: {payload!r}"
            time.sleep(0.1)
        return None, f"{self.label}: timeout ({timeout}s) waiting for WALLHACK_RESULT"

    def poll(self) -> int | None:
        return self._proc.poll()

    def log_tail(self, n: int = 50) -> str:
        lines = list(self._log)
        return "\n".join(lines[-n:])

    # ── cleanup ──────────────────────────────────────────────────────────────

    def kill(self):
        try:
            pgid = os.getpgid(self._proc.pid)
            os.killpg(pgid, signal.SIGTERM)
        except (OSError, ProcessLookupError):
            pass
        try:
            self._proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                pgid = os.getpgid(self._proc.pid)
                os.killpg(pgid, signal.SIGKILL)
            except (OSError, ProcessLookupError):
                pass

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.kill()

# ── QEMU command builder ──────────────────────────────────────────────────────

def _qemu_base(net_fd: int, role: str, cmdline_params: str,
               verbose_wallhack: bool) -> list[str]:
    """Return the base qemu-system-x86_64 argument list."""
    kernel_append = f"console=ttyS0 net.ifnames=0 biosdevname=0 {cmdline_params}"
    if verbose_wallhack:
        kernel_append += " wallhack.debug=1"

    return [
        QEMU,
        "-enable-kvm",
        "-snapshot",
        "-m", "512M",
        "-smp", "2",
        "-drive", f"file={VM_IMAGE},if=virtio,format=qcow2",
        "-netdev", f"socket,id=net0,fd={net_fd}",
        "-device", "virtio-net-pci,netdev=net0",
        "-virtfs", f"local,path={REPO_ROOT},mount_tag=wallhack,security_model=none,readonly=on",
        "-nographic",
        "-no-reboot",
        "-append", f"init=/usr/local/bin/wallhack-init {kernel_append}",
    ]

# ── scenario runner ───────────────────────────────────────────────────────────

class ScenarioResult:
    def __init__(self, name: str, transport: str):
        self.name = name
        self.transport = transport
        self.passed = False
        self.reason = ""
        self.duration_s: float = 0.0
        self.exit_log = ""
        self.entry_log = ""

    def __str__(self):
        icon = "PASS" if self.passed else "FAIL"
        base = f"[{icon}] {self.name}/{self.transport}"
        if self.duration_s:
            base += f"  ({self.duration_s:.1f}s)"
        if not self.passed:
            base += f"\n       reason: {self.reason}"
        return base


def run_scenario(
    scenario: str,
    transport: str,
    netem: dict | None = None,
    verbose: bool = False,
    debug_wallhack: bool = False,
    keep_running: bool = False,
) -> ScenarioResult:
    """Boot two VMs and run one test scenario.  Returns a ScenarioResult."""
    result = ScenarioResult(scenario, transport)
    start = time.monotonic()

    # Build kernel cmdline params for each role
    extra = f"wallhack.scenario={scenario} wallhack.transport={transport}"
    if netem:
        if "loss"  in netem: extra += f" wallhack.loss={netem['loss']}"
        if "delay" in netem: extra += f" wallhack.delay={netem['delay']}"
    if keep_running:
        extra += " wallhack.debug=1"

    exit_params  = f"wallhack.role=exit  {extra}"
    entry_params = f"wallhack.role=entry {extra}"

    # Create a socketpair for the inter-VM L2 link (no host port needed)
    sock_a, sock_b = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)

    with contextlib.ExitStack() as stack:
        # Boot exit VM first (it binds the L2 socket half)
        exit_vm = stack.enter_context(VMProcess(
            _qemu_base(sock_a.fileno(), "exit", exit_params, debug_wallhack),
            label="exit",
            pass_fds=(sock_a.fileno(),),
        ))
        sock_a.close()  # parent no longer needs its end

        # Wait for exit VM to be ready
        _, err = exit_vm.wait_for("WALLHACK_EXIT_READY", VM_TIMEOUT_EXIT_READY)
        if err:
            result.reason = err
            result.exit_log = exit_vm.log_tail()
            result.duration_s = time.monotonic() - start
            _save_logs(scenario, transport, result)
            return result

        # Boot entry VM
        entry_vm = stack.enter_context(VMProcess(
            _qemu_base(sock_b.fileno(), "entry", entry_params, debug_wallhack),
            label="entry",
            pass_fds=(sock_b.fileno(),),
        ))
        sock_b.close()

        # debug-topology: user watches both streams; Ctrl-C kills VMs
        if keep_running:
            print(f"[debug-topology] Both VMs running. Ctrl-C to stop.")
            try:
                while exit_vm.poll() is None and entry_vm.poll() is None:
                    time.sleep(0.5)
            except KeyboardInterrupt:
                pass
            return result

        # Poll both VMs every 100 ms; fail immediately if either crashes
        outcome, err = entry_vm.wait_for_result(VM_TIMEOUT_RESULT)
        if err:
            result.reason = err
        elif outcome and outcome.get("status") == "pass":
            result.passed = True
            result.duration_s = float(outcome.get("duration_s", 0))
        else:
            result.reason = outcome.get("reason", "unknown") if outcome else "no result"
            result.duration_s = float((outcome or {}).get("duration_s", 0))

        result.exit_log  = exit_vm.log_tail()
        result.entry_log = entry_vm.log_tail()

    if not result.duration_s:
        result.duration_s = time.monotonic() - start

    _save_logs(scenario, transport, result)
    return result


def _save_logs(scenario: str, transport: str, result: ScenarioResult):
    """Write ring buffer logs to results/logs/<timestamp>/ on failure."""
    if result.passed:
        return
    ts = time.strftime("%Y%m%d-%H%M%S")
    log_dir = LOG_DIR / f"{ts}-{scenario}-{transport}"
    log_dir.mkdir(parents=True, exist_ok=True)
    (log_dir / "exit.log").write_text(result.exit_log)
    (log_dir / "entry.log").write_text(result.entry_log)


# ── display helpers ───────────────────────────────────────────────────────────

def _show_result(result: ScenarioResult, verbose: bool, debug: bool):
    print(str(result))
    if (not result.passed and verbose) or debug:
        if result.entry_log:
            print(f"  --- entry VM (last {RING_BUFFER_SIZE} lines) ---")
            print(result.entry_log)
        if result.exit_log:
            print(f"  --- exit VM (last {RING_BUFFER_SIZE} lines) ---")
            print(result.exit_log)

# ── smoke suite ───────────────────────────────────────────────────────────────

SMOKE_SCENARIOS = [
    ("smoke", "quic",      None),
    ("smoke", "websocket", None),
]

RESILIENCE_SCENARIOS = [
    ("resilience", "quic",      {"loss": "0.5%", "delay": "5ms"}),
    ("resilience", "quic",      {"loss": "2%",   "delay": "25ms"}),
    ("resilience", "quic",      {"loss": "0%",   "delay": "100ms"}),
    ("resilience", "websocket", {"loss": "0.5%", "delay": "5ms"}),
    ("resilience", "websocket", {"loss": "2%",   "delay": "25ms"}),
    ("resilience", "websocket", {"loss": "0%",   "delay": "100ms"}),
]

def _filter_by_transport(scenarios, transport_filter):
    if transport_filter == "both":
        return scenarios
    return [(s, t, n) for s, t, n in scenarios if t == transport_filter]

# ── main ──────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="wallhack integration test runner")
    sub = parser.add_subparsers(dest="cmd", required=True)

    for name in ("smoke", "resilience"):
        sp = sub.add_parser(name)
        sp.add_argument("--transport", choices=["quic", "websocket", "both"],
                        default="both")
        sp.add_argument("--verbose", action="store_true")
        sp.add_argument("--debug",   action="store_true",
                        help="Pass --debug to wallhack; print full logs always")

    dt = sub.add_parser("debug-topology")
    dt.add_argument("--transport", choices=["quic", "websocket"], default="quic")

    args = parser.parse_args()
    preflight()

    signal.signal(signal.SIGINT, lambda _s, _f: sys.exit(130))

    if args.cmd == "debug-topology":
        run_scenario(
            "debug-topology", args.transport,
            keep_running=True, debug_wallhack=True,
        )
        return

    scenarios_map = {"smoke": SMOKE_SCENARIOS, "resilience": RESILIENCE_SCENARIOS}
    scenarios = _filter_by_transport(scenarios_map[args.cmd], args.transport)

    results = []
    for scenario, transport, netem in scenarios:
        label = f"{scenario}/{transport}"
        if netem:
            label += " (" + " ".join(f"{k}={v}" for k, v in netem.items()) + ")"
        print(f"Running {label}...", end=" ", flush=True)
        result = run_scenario(
            scenario, transport, netem=netem,
            verbose=args.verbose,
            debug_wallhack=getattr(args, "debug", False),
        )
        results.append(result)
        _show_result(result, getattr(args, "verbose", False),
                     getattr(args, "debug", False))

    passed = sum(1 for r in results if r.passed)
    total  = len(results)
    print(f"\n{passed}/{total} scenarios passed.")

    if passed < total:
        sys.exit(1)


if __name__ == "__main__":
    main()
