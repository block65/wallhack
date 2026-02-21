#!/usr/bin/env python3
"""wallhack benchmark runner.

Usage:
    python3 bench/run_benchmarks.py [--runs N] [--transport quic|websocket|both]
                                    [--output DIR]

Metrics per transport:
    tcp_fwd   — TCP forward throughput (iperf3 client → server)
    tcp_rev   — TCP reverse throughput (iperf3 server → client)
    udp       — UDP throughput
    latency   — ICMP RTT
    parallel2 — TCP with 2 parallel streams
    parallel4 — TCP with 4 parallel streams

Results are written to results/<timestamp>.json and results/latest -> that file.
"""

import argparse
import collections
import contextlib
import json
import os
import pathlib
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time

REPO_ROOT  = pathlib.Path(__file__).resolve().parent.parent
VM_IMAGE   = REPO_ROOT / "bench" / "vm" / "base.qcow2"
IPERF3_BIN = REPO_ROOT / "bench" / "bin" / "iperf3"
RESULTS_DIR = REPO_ROOT / "bench" / "results"

QEMU     = "qemu-system-x86_64"
RING_BUFFER_SIZE      = 500
VM_TIMEOUT_EXIT_READY = 60
VM_TIMEOUT_RESULT     = 120  # benchmarks take longer than smoke tests

METRICS = ["tcp_fwd", "tcp_rev", "udp", "latency", "parallel2", "parallel4"]
TRANSPORTS = ["quic", "websocket"]

# ── preflight ─────────────────────────────────────────────────────────────────

def preflight():
    errors = []
    if not os.access("/dev/kvm", os.R_OK | os.W_OK):
        errors.append("Error: /dev/kvm not accessible. Add yourself to the kvm group.")
    if subprocess.run(["which", QEMU], capture_output=True).returncode != 0:
        errors.append(f"Error: {QEMU} not found. Install qemu-system-x86.")
    if not VM_IMAGE.exists():
        errors.append("Error: VM image not found. Run: just setup-vm")
    if not IPERF3_BIN.exists():
        errors.append("Error: iperf3 not found. Run: just fetch-iperf3")
    if errors:
        for e in errors:
            print(e, file=sys.stderr)
        sys.exit(1)

# ── VMProcess (same as run_tests.py — stdlib only, no shared module) ──────────

class VMProcess:
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
        self._thread = threading.Thread(target=self._drain, daemon=True,
                                        name=f"drain-{label}")
        self._thread.start()

    def _drain(self):
        for raw in self._proc.stdout:
            self._log.append(raw.decode(errors="replace").rstrip())

    def wait_for(self, pattern: str, timeout: float) -> tuple[str | None, str | None]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._proc.poll() is not None:
                return None, f"{self.label}: process exited with code {self._proc.returncode}"
            for line in list(self._log):
                if pattern in line:
                    return line, None
            time.sleep(0.1)
        return None, f"{self.label}: timeout ({timeout}s) waiting for {pattern!r}"

    def wait_for_result(self, timeout: float) -> tuple[dict | None, str | None]:
        prefix = "WALLHACK_RESULT: "
        deadline = time.monotonic() + timeout
        seen: set[str] = set()
        while time.monotonic() < deadline:
            rc = self._proc.poll()
            if rc is not None and rc != 0:
                return None, f"{self.label}: exited with code {rc} before result"
            for line in list(self._log):
                if line.startswith(prefix) and line not in seen:
                    seen.add(line)
                    try:
                        return json.loads(line[len(prefix):]), None
                    except json.JSONDecodeError as exc:
                        return None, f"malformed JSON: {exc}"
            time.sleep(0.1)
        return None, f"{self.label}: timeout ({timeout}s) waiting for WALLHACK_RESULT"

    def poll(self) -> int | None:
        return self._proc.poll()

    def log_tail(self, n: int = 50) -> str:
        return "\n".join(list(self._log)[-n:])

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

    def __enter__(self): return self
    def __exit__(self, *_): self.kill()


# ── QEMU command builder ──────────────────────────────────────────────────────

def _qemu_base(net_fd: int, cmdline_params: str) -> list[str]:
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
        "-append", f"init=/usr/local/bin/wallhack-init console=ttyS0 net.ifnames=0 biosdevname=0 {cmdline_params}",
    ]


# ── single metric run ─────────────────────────────────────────────────────────

def run_one(metric: str, transport: str, netem: dict | None = None
            ) -> float | None:
    """Boot a VM pair and measure one metric.  Returns the value or None."""
    extra = f"wallhack.scenario=benchmark wallhack.transport={transport} wallhack.metric={metric}"
    if netem:
        if "loss"  in netem: extra += f" wallhack.loss={netem['loss']}"
        if "delay" in netem: extra += f" wallhack.delay={netem['delay']}"

    sock_a, sock_b = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)

    with contextlib.ExitStack() as stack:
        exit_vm = stack.enter_context(VMProcess(
            _qemu_base(sock_a.fileno(), f"wallhack.role=exit  {extra}"),
            label="exit", pass_fds=(sock_a.fileno(),),
        ))
        sock_a.close()

        _, err = exit_vm.wait_for("WALLHACK_EXIT_READY", VM_TIMEOUT_EXIT_READY)
        if err:
            print(f"    exit VM error: {err}", file=sys.stderr)
            return None

        entry_vm = stack.enter_context(VMProcess(
            _qemu_base(sock_b.fileno(), f"wallhack.role=entry {extra}"),
            label="entry", pass_fds=(sock_b.fileno(),),
        ))
        sock_b.close()

        outcome, err = entry_vm.wait_for_result(VM_TIMEOUT_RESULT)
        if err:
            print(f"    {err}", file=sys.stderr)
            print(f"    --- entry tail ---\n{entry_vm.log_tail()}", file=sys.stderr)
            print(f"    --- exit tail ---\n{exit_vm.log_tail()}", file=sys.stderr)
            return None

    if not outcome or outcome.get("status") != "pass":
        reason = (outcome or {}).get("reason", "unknown")
        print(f"    FAIL: {reason}", file=sys.stderr)
        return None

    if metric == "latency":
        return float(outcome.get("value_ms", 0))
    return float(outcome.get("value_mbps", 0))


# ── wallhack version ──────────────────────────────────────────────────────────

def _wallhack_version() -> str:
    wh = REPO_ROOT / "target" / "release" / "wallhack"
    if not wh.exists():
        return "unknown"
    r = subprocess.run([str(wh), "--version"], capture_output=True, text=True)
    return r.stdout.strip().split()[-1] if r.returncode == 0 else "unknown"


# ── main ──────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="wallhack benchmark runner")
    parser.add_argument("--runs", type=int, default=3, metavar="N",
                        help="Number of runs per scenario (default: 3)")
    parser.add_argument("--transport", choices=["quic", "websocket", "both"],
                        default="both")
    parser.add_argument("--output", type=pathlib.Path, default=None,
                        help="Directory for result JSON (default: bench/results/)")
    args = parser.parse_args()

    preflight()
    signal.signal(signal.SIGINT, lambda _s, _f: sys.exit(130))

    transports = TRANSPORTS if args.transport == "both" else [args.transport]
    output_dir = args.output or RESULTS_DIR
    output_dir.mkdir(parents=True, exist_ok=True)

    wh_version = _wallhack_version()
    timestamp  = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    scenarios_out = []

    for transport in transports:
        for metric in METRICS:
            name = f"{metric}/{transport}"
            print(f"  {name} ({args.runs} runs)...", end="", flush=True)
            runs: list[float] = []

            for i in range(args.runs):
                print(f" run{i+1}", end="", flush=True)
                val = run_one(metric, transport)
                if val is not None:
                    runs.append(val)
                else:
                    print(f" [failed]", end="", flush=True)

            if runs:
                runs.sort()
                median = statistics.median(runs)
                unit = "ms" if metric == "latency" else "Mbps"
                print(f"  min={runs[0]:.1f} median={median:.1f} max={runs[-1]:.1f} {unit}")
                scenarios_out.append({
                    "name":      name,
                    "transport": transport,
                    "metric":    "latency_ms" if metric == "latency" else "throughput_mbps",
                    "runs":      runs,
                    "min":       runs[0],
                    "median":    float(median),
                    "max":       runs[-1],
                })
            else:
                print("  all runs failed")

    result_obj = {
        "version":           1,
        "timestamp":         timestamp,
        "wallhack_version":  wh_version,
        "scenarios":         scenarios_out,
    }

    ts_slug  = time.strftime("%Y%m%d-%H%M%S")
    out_path = output_dir / f"{ts_slug}.json"
    out_path.write_text(json.dumps(result_obj, indent=2))

    latest = output_dir / "latest"
    if latest.is_symlink() or latest.exists():
        latest.unlink()
    latest.symlink_to(out_path.name)

    print(f"\nResults written to {out_path}")
    print(f"Symlink:           {latest} -> {out_path.name}")


if __name__ == "__main__":
    main()
