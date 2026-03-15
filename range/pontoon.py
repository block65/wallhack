#!/usr/bin/env python3
"""Pontoon — QEMU microVM range orchestrator for wallhack."""

import argparse
import getpass
import glob
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

PONTOON_DIR = "/tmp/pontoon"
STATE_FILE = f"{PONTOON_DIR}/state.json"
QEMU = "qemu-system-x86_64"


# ── compose parsing ────────────────────────────────────────────────────────────


def parse_compose(path: str) -> dict:
    try:
        import yaml
    except ImportError:
        print("error: pyyaml not installed (pip install pyyaml)", file=sys.stderr)
        sys.exit(1)
    with open(path) as f:
        raw = yaml.safe_load(f)
    base = Path(path).parent
    defaults = raw.get("defaults", {})
    # Resolve kernel/initrd paths relative to compose.yaml location
    if "kernel" in defaults:
        defaults["kernel"] = str((base / defaults["kernel"]).resolve())
    if "initrd" in defaults:
        defaults["initrd"] = str((base / defaults["initrd"]).resolve())
    return {
        "defaults": defaults,
        "networks": raw.get("networks", {}),
        "services": raw.get("services", {}),
        "_base": str(base),
    }


def parse_memory(mem_str: str) -> int:
    """Parse memory string like '256m' or '128M' to integer MB."""
    s = str(mem_str).strip().lower()
    if s.endswith("m"):
        return int(s[:-1])
    if s.endswith("g"):
        return int(s[:-1]) * 1024
    return int(s)


# ── naming helpers ─────────────────────────────────────────────────────────────


def mac_for_vm(vm_name: str, iface_idx: int) -> str:
    h = hashlib.sha1(vm_name.encode()).digest()
    return f"52:54:00:{h[0]:02x}:{h[1]:02x}:{iface_idx + 1:02x}"


def tap_name(vm_name: str, iface_idx: int) -> str:
    # Linux interface name limit: 15 chars.
    # Use a 2-hex-char hash suffix to disambiguate names that share the same
    # 6-char prefix (e.g. "gateway-perimeter" vs "gateway-office").
    # Format: "tap-" (4) + slug[:5] (5) + h2 (2) + "-e" (2) + digit (1) = 14 chars max.
    h = hashlib.sha1(vm_name.encode()).digest()
    h2 = f"{h[0]:02x}"
    slug = vm_name.replace("-", "")[:5]
    return f"tap-{slug}{h2}-e{iface_idx}"


def console_sock(vm_name: str) -> str:
    return f"{PONTOON_DIR}/{vm_name}-console.sock"


def mcp_sock(vm_name: str) -> str:
    return f"{PONTOON_DIR}/{vm_name}-mcp.sock"


def vm_log(vm_name: str) -> str:
    return f"{PONTOON_DIR}/{vm_name}.log"


def iface_exists(name: str) -> bool:
    return os.path.exists(f"/sys/class/net/{name}")


# ── state management ───────────────────────────────────────────────────────────


def load_state() -> dict:
    if not os.path.exists(STATE_FILE):
        return {}
    with open(STATE_FILE) as f:
        return json.load(f)


def save_state(state: dict) -> None:
    os.makedirs(PONTOON_DIR, exist_ok=True)
    with open(STATE_FILE, "w") as f:
        json.dump(state, f, indent=2)


# ── process management ─────────────────────────────────────────────────────────


def kill_gracefully(pid: int, timeout: float = 3.0) -> None:
    try:
        os.kill(pid, signal.SIGTERM)
    except (ProcessLookupError, OSError):
        return
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            os.kill(pid, 0)
        except (ProcessLookupError, OSError):
            return
        time.sleep(0.1)
    try:
        os.kill(pid, signal.SIGKILL)
    except (ProcessLookupError, OSError):
        pass


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except (ProcessLookupError, OSError):
        return False


# ── network setup ──────────────────────────────────────────────────────────────


def _ip(*args: str, check: bool = True, **kwargs) -> subprocess.CompletedProcess:
    """Run an ip command. Requires root (run pontoon with sudo)."""
    if os.getuid() != 0:
        print("error: network setup requires root — run with sudo", file=sys.stderr)
        sys.exit(1)
    return subprocess.run(["ip", *args], check=check, **kwargs)


def setup_network(cfg: dict) -> None:
    # When running under sudo, create TAPs owned by the original user
    # so QEMU (which drops back to that user) can open them.
    user = os.environ.get("SUDO_USER") or getpass.getuser()
    for net_name, net_cfg in cfg["networks"].items():
        br = f"br-{net_name}"
        subnet = net_cfg.get("subnet", "")
        if not iface_exists(br):
            _ip("link", "add", br, "type", "bridge")
            _ip("link", "set", br, "up")
            if subnet and not net_cfg.get("internal"):
                # Assign .254 host address so the host can reach VMs directly
                prefix = subnet.split("/")[0].rsplit(".", 1)[0]
                cidr = subnet.split("/")[1]
                _ip("addr", "add", f"{prefix}.254/{cidr}", "dev", br,
                    check=False, capture_output=True)

    for vm_name, svc in cfg["services"].items():
        networks = svc.get("networks", {})
        for iface_idx, (net_name, _) in enumerate(networks.items()):
            tap = tap_name(vm_name, iface_idx)
            br = f"br-{net_name}"
            if not iface_exists(tap):
                # user= makes the tap accessible to the current user so QEMU
                # runs unprivileged
                _ip("tuntap", "add", "dev", tap, "mode", "tap", "user", user)
                _ip("link", "set", tap, "master", br)
                _ip("link", "set", tap, "up")


def teardown_network(cfg: dict) -> None:
    for vm_name, svc in cfg["services"].items():
        for iface_idx in range(len(svc.get("networks", {}))):
            tap = tap_name(vm_name, iface_idx)
            if iface_exists(tap):
                _ip("link", "del", tap, check=False, capture_output=True)

    for net_name in cfg["networks"]:
        br = f"br-{net_name}"
        if iface_exists(br):
            _ip("link", "del", br, check=False, capture_output=True)


# ── QEMU command builder ───────────────────────────────────────────────────────


def build_kernel_cmdline(vm_name: str, svc: dict, cfg: dict) -> str:
    networks = svc.get("networks", {})
    kernel_args = svc.get("kernel_args", {})

    # Build net.ip= from ordered interface list
    ip_entries = []
    masquerade_iface = None
    gateway = None
    has_multiple_ifaces = len(networks) >= 2

    for iface_idx, (net_name, net_iface_cfg) in enumerate(networks.items()):
        eth = f"eth{iface_idx}"
        net_iface_cfg = net_iface_cfg or {}
        addr = net_iface_cfg.get("ipv4_address", "")
        net_subnet = cfg["networks"].get(net_name, {}).get("subnet", "")
        cidr = net_subnet.split("/")[1] if "/" in net_subnet else "24"
        if addr:
            ip_entries.append(f"{eth}:{addr}/{cidr}")
        if net_iface_cfg.get("masquerade"):
            masquerade_iface = eth
        if "gateway" in net_iface_cfg:
            gateway = net_iface_cfg["gateway"]

    parts = [
        "console=ttyS0",
        "net.ifnames=0",
        "rdinit=/init",
    ]

    if ip_entries:
        parts.append(f"net.ip={','.join(ip_entries)}")

    if gateway:
        parts.append(f"net.gw={gateway}")

    if has_multiple_ifaces:
        parts.append("net.forward=1")

    if masquerade_iface:
        parts.append(f"net.masquerade={masquerade_iface}")

    # Append all kernel_args from compose
    for key, val in kernel_args.items():
        parts.append(f"{key}={val}")

    return " ".join(parts)


# CIDs 0=hypervisor, 1=local, 2=host — guests start at 3.
VSOCK_CID_BASE = 3


def cid_for_vm(cfg: dict, vm_name: str) -> int:
    """Return the vsock CID for a VM (deterministic, based on service order)."""
    return VSOCK_CID_BASE + list(cfg["services"].keys()).index(vm_name)


def build_qemu_cmd(vm_name: str, svc: dict, cfg: dict) -> list[str]:
    defaults = cfg.get("defaults", {})
    kernel = svc.get("kernel") or defaults.get("kernel", "")
    # Per-VM initrd (from 'just build') takes precedence over the shared default
    per_vm = Path(cfg["_base"]) / "vm" / "build" / f"initrd-{vm_name}.gz"
    initrd = str(per_vm) if per_vm.exists() else (svc.get("initrd") or defaults.get("initrd", ""))
    memory_mb = parse_memory(svc.get("memory", "256m"))
    cpus = svc.get("cpus", 1)
    networks = svc.get("networks", {})
    share_dir = f"{PONTOON_DIR}/share/{vm_name}"
    cid = cid_for_vm(cfg, vm_name)

    cmdline = build_kernel_cmdline(vm_name, svc, cfg)

    netdev_args = []
    for iface_idx, net_name in enumerate(networks.keys()):
        tap = tap_name(vm_name, iface_idx)
        mac = mac_for_vm(vm_name, iface_idx)
        netdev_id = f"net{iface_idx}"
        netdev_args += [
            "-netdev", f"tap,id={netdev_id},ifname={tap},script=no,downscript=no",
            "-device", f"virtio-net-device,netdev={netdev_id},mac={mac}",
        ]

    sock = console_sock(vm_name)
    mcp = mcp_sock(vm_name)
    log = vm_log(vm_name)

    return [
        QEMU,
        "-M", "microvm,acpi=off,pit=off,pic=off,rtc=off",
        "-enable-kvm",
        "-cpu", "host",
        "-m", str(memory_mb),
        "-smp", str(cpus),
        "-kernel", kernel,
        "-initrd", initrd,
        "-nographic",
        "-no-reboot",
        # Serial console (ttyS0) → unix socket + logfile (human dashboard + logs)
        "-chardev", f"socket,id=char0,path={sock},server=on,wait=off,logfile={log}",
        "-serial", "chardev:char0",
        # Secondary serial (virtio-console hvc0) -> unix socket (MCP agent exclusive)
        "-chardev", f"socket,id=char1,path={mcp},server=on,wait=off",
        "-device", "virtio-serial-device",
        "-device", "virtconsole,chardev=char1",
        # 9p virtio share for vm_inject
        "-fsdev", f"local,id=fs0,path={share_dir},security_model=none",
        "-device", "virtio-9p-device,fsdev=fs0,mount_tag=hostshare",
        *netdev_args,
        "-device", f"vhost-vsock-device,guest-cid={cid}",
        "-append", cmdline,
    ]


# ── VM launch ─────────────────────────────────────────────────────────────────


def _drop_privs_preexec():
    """If running as root via sudo, drop back to the original user for QEMU."""
    sudo_uid = os.environ.get("SUDO_UID")
    sudo_gid = os.environ.get("SUDO_GID")
    if os.getuid() == 0 and sudo_uid:
        os.setgid(int(sudo_gid or sudo_uid))
        os.setuid(int(sudo_uid))


def launch_vms(cfg: dict) -> dict[str, int]:
    pids = {}
    for vm_name, svc in cfg["services"].items():
        share_dir = f"{PONTOON_DIR}/share/{vm_name}"
        os.makedirs(share_dir, exist_ok=True)

        cmd = build_qemu_cmd(vm_name, svc, cfg)

        qemu_log = vm_log(vm_name) + ".qemu"
        with open(qemu_log, "w") as qemu_log_f:
            proc = subprocess.Popen(
                cmd,
                stdin=subprocess.DEVNULL,
                stdout=qemu_log_f,
                stderr=subprocess.STDOUT,
                start_new_session=True,
                preexec_fn=_drop_privs_preexec,
            )
        pids[vm_name] = proc.pid
        print(f"  [{vm_name}] launched (pid={proc.pid})", flush=True)
    return pids


# ── boot readiness ────────────────────────────────────────────────────────────


def wait_ready(vm_names: list[str], timeout: int = 90) -> None:
    # Monitor log files for BOOT_COMPLETE_V2
    ready: set[str] = set()
    deadline = time.time() + timeout

    # Open file handles for reading
    fds = {}
    
    try:
        while time.time() < deadline and ready != set(vm_names):
            # Check for new log files
            for vm in set(vm_names) - ready:
                if vm not in fds:
                    log_path = vm_log(vm)
                    if os.path.exists(log_path):
                         try:
                             fds[vm] = open(log_path, "r", errors="replace")
                         except OSError:
                             pass
                
                if vm in fds:
                    try:
                        # Read all available new lines
                        lines = fds[vm].read()
                        if "BOOT_COMPLETE_V2" in lines:
                            ready.add(vm)
                            print(f"  [{vm}] ready", flush=True)
                    except OSError:
                        pass
            
            time.sleep(0.5)
    finally:
        for f in fds.values():
            try:
                f.close()
            except OSError:
                pass

    not_ready = set(vm_names) - ready
    if not_ready:
        raise RuntimeError(f"VMs failed to boot in {timeout}s: {not_ready}")


# ── serial console interaction ─────────────────────────────────────────────────


def mcp_audit_log(vm_name: str, direction: str, content: str) -> None:
    log_path = os.path.join(PONTOON_DIR, "mcp-audit.log")
    timestamp = time.strftime("%H:%M:%S")
    # No truncation for better visibility
    display_content = content.strip()
    
    try:
        with open(log_path, "a") as f:
            f.write(f"[{timestamp}] [{vm_name}] {direction} {display_content}\n")
    except OSError:
        pass


def serial_send_recv(vm_name: str, command: str, timeout_s: int = 30) -> str:
    sock_path = mcp_sock(vm_name)
    
    mcp_audit_log(vm_name, "REQ", command)
    
    sentinel = f"__PONTOON_END_{int(time.time())}__"
    
    # We want to catch Ctrl-C and forward it to the VM
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.connect(sock_path)
        s.settimeout(0.1)
        
        # Ensure we are in a clean state:
        # Send a newline to clear any partial input
        s.sendall(b"\n")
        time.sleep(0.1)
        
        # Drain any pending output
        try:
            while True:
                chunk = s.recv(4096)
                if not chunk: break
        except socket.timeout:
            pass
        except OSError:
            pass

        # Robust command execution:
        # 1. Disable echo to prevent command leakage into logs/output
        # 2. Set TERM=dumb to prevent escape codes
        # 3. Use a sentinel for end detection
        # 4. Capture return code
        full_cmd = (
            f"export TERM=dumb; "
            f"stty -echo 2>/dev/null; "
            f"export PATH=/mnt/share/{vm_name}:/usr/local/bin:/usr/sbin:/sbin:/usr/bin:/bin; "
            f"{command}; "
            f"RET=$?; "
            f"stty echo 2>/dev/null; " # Re-enable echo for next interactive use
            f"echo \"{sentinel}:$RET\"\n"
        )
        s.sendall(full_cmd.encode())

        buf = b""
        deadline = time.time() + timeout_s
        
        while time.time() < deadline:
            try:
                chunk = s.recv(4096)
                if chunk:
                    buf += chunk
                    decoded = buf.decode(errors="replace")
                    
                    # Filter out common CPR (Cursor Position Report) noise
                    # ESC [ n ; m R
                    if "\x1b[" in decoded and "R" in decoded:
                         import re
                         decoded = re.sub(r'\x1b\[\d+;\d+R', '', decoded)

                    # Check for sentinel with return code (avoid matching the command echo)
                    # The command echo contains: echo "SENTINEL:$RET"
                    # The actual output contains: SENTINEL:0
                    # So we look for SENTINEL followed by a digit.
                    if sentinel + ":" in decoded:
                        # Find the last occurrence or use regex?
                        # Using regex is safer to confirm it's followed by a digit
                        import re
                        match = re.search(re.escape(sentinel) + r':(\d+)', decoded)
                        if match:
                             # We found the real completion
                             end_idx = match.start()
                             output = decoded[:end_idx]
                             # Also might want to capture the ret code if needed, but not used currently
                             output = output.strip()
                             mcp_audit_log(vm_name, "RES", output)
                             return output
            except socket.timeout:
                pass
            except KeyboardInterrupt:
                # User hit Ctrl-C. Send SIGINT to VM.
                print(f"\n[pontoon] sending interrupt to {vm_name}...", file=sys.stderr)
                s.sendall(b"\x03") # Ctrl-C
                # Give it a moment to interrupt
                time.sleep(0.5)
                # We should probably return what we have or raise?
                # The caller (script) might want to know it was interrupted.
                raise

        # Timeout
        # Try to interrupt the command in the VM so it doesn't run forever
        s.sendall(b"\x03")
        time.sleep(0.5)
        raise TimeoutError(f"vm_exec timeout after {timeout_s}s on {vm_name}")

    finally:
        s.close()


def serial_read_duration(vm_name: str, duration_s: int) -> str:
    sock_path = console_sock(vm_name)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.connect(sock_path)
        s.settimeout(0.1)
        buf = b""
        deadline = time.time() + duration_s
        while time.time() < deadline:
            try:
                chunk = s.recv(4096)
                if chunk:
                    buf += chunk
            except socket.timeout:
                pass
        return buf.decode(errors="replace")


# ── layer build ────────────────────────────────────────────────────────────────


def _gen_dockerfile(alpine_tag: str, packages: list[str], file_copies: list[tuple[str, str]], chmod_paths: list[str]) -> str:
    lines = [f"FROM alpine:{alpine_tag}"]
    lines.append("RUN mkdir -p /mnt/share /svc")
    if packages:
        pkg_str = " \\\n        ".join(packages)
        lines.append(f"RUN apk add --no-cache \\\n        {pkg_str}")
    for ctx_src, img_dest in file_copies:
        lines.append(f"COPY {ctx_src} {img_dest}")
    if chmod_paths:
        lines.append(f"RUN chmod +x {' '.join(chmod_paths)}")
    return "\n".join(lines) + "\n"


def cmd_build(cfg: dict, base_dir: Path) -> None:
    try:
        import yaml
    except ImportError:
        print("error: pyyaml not installed (pip install pyyaml)", file=sys.stderr)
        sys.exit(1)

    layers_dir = base_dir / "layers"
    build_dir = base_dir / "vm" / "build"
    build_dir.mkdir(parents=True, exist_ok=True)
    alpine_tag = cfg.get("defaults", {}).get("alpine", "3.21")

    for vm_name, svc in cfg["services"].items():
        layer_names = svc.get("layers", [])
        if not layer_names:
            continue

        print(f"build: [{vm_name}] layers={layer_names}")

        # Load all layer configs
        layers = []
        for name in layer_names:
            layer_file = layers_dir / name / "layer.yml"
            with open(layer_file) as f:
                layer_cfg = yaml.safe_load(f) or {}
            layer_cfg["_dir"] = layers_dir / name
            layer_cfg["_name"] = name
            layers.append(layer_cfg)

        # Packages: sorted across all layers for deterministic Docker cache key
        all_packages = sorted({
            pkg
            for layer in layers
            for pkg in layer.get("packages", [])
        })

        # Assemble build context
        ctx_dir = build_dir / f"ctx-{vm_name}"
        if ctx_dir.exists():
            shutil.rmtree(ctx_dir)
        ctx_dir.mkdir(parents=True)

        file_copies: list[tuple[str, str]] = []  # (ctx-relative-src, image-abs-dest)
        chmod_paths: list[str] = []

        # init.sh → /init
        init_src = base_dir / "vm" / "init.sh"
        shutil.copy2(init_src, ctx_dir / "init")
        file_copies.append(("init", "/init"))
        chmod_paths.append("/init")

        # Process layers in order — later layer wins for same dest (config override)
        seen: dict[str, str] = {}  # img_dest → ctx_rel_src

        for layer in layers:
            layer_dir: Path = layer["_dir"]
            name = layer["_name"]

            # Config overlays
            for conf in layer.get("configs", []):
                src = layer_dir / conf
                ctx_rel = f"configs/{name}/{conf}"
                dest_in_ctx = ctx_dir / ctx_rel
                dest_in_ctx.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src, dest_in_ctx)
                seen[f"/{conf}"] = ctx_rel

            # Service start script
            start = layer.get("start")
            if start:
                ctx_rel = f"services/{name}/start.sh"
                dest_in_ctx = ctx_dir / ctx_rel
                dest_in_ctx.parent.mkdir(parents=True, exist_ok=True)
                dest_in_ctx.write_text(f"#!/bin/sh\n{start}")
                img_dest = f"/svc/{name}/start.sh"
                seen[img_dest] = ctx_rel
                chmod_paths.append(img_dest)

            # Binary injection (e.g. wallhack from cargo)
            binary = layer.get("binary")
            if binary:
                src_rel, img_dest = [s.strip() for s in binary.split("->")]
                src_abs = (base_dir.parent / src_rel).resolve()
                if not src_abs.exists():
                    print(f"  [{vm_name}] warning: binary not found: {src_abs}", file=sys.stderr)
                    print(f"  [{vm_name}] hint: run 'just range::build-wallhack' first", file=sys.stderr)
                    sys.exit(1)
                ctx_rel = f"binaries/{name}/{src_abs.name}"
                dest_in_ctx = ctx_dir / ctx_rel
                dest_in_ctx.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src_abs, dest_in_ctx)
                seen[img_dest] = ctx_rel
                chmod_paths.append(img_dest)

        # Emit file_copies in deterministic order (sorted by image dest)
        file_copies += [(ctx_rel, img_dest) for img_dest, ctx_rel in sorted(seen.items())]

        # Generate and write Dockerfile
        dockerfile = _gen_dockerfile(alpine_tag, all_packages, file_copies, chmod_paths)
        (ctx_dir / "Dockerfile").write_text(dockerfile)

        # docker build → extract rootfs
        rootfs_dir = build_dir / f"rootfs-{vm_name}"
        subprocess.run([
            "docker", "build",
            "--output", f"type=local,dest={rootfs_dir}",
            str(ctx_dir),
        ], check=True)

        # Fix root dir permissions: docker --output sets the extracted rootfs_dir
        # to the host user's uid with mode 700, which would make / inside the VM
        # drwx------ (root:root via --owner 0:0 but still mode 700). cpio
        # --owner only rewrites uid/gid, not mode — chmod the dir first.
        rootfs_dir.chmod(0o755)

        # Pack initrd
        initrd_path = build_dir / f"initrd-{vm_name}.gz"
        subprocess.run(
            f"(cd '{rootfs_dir}' && find . | cpio -o -H newc --quiet --owner 0:0) | gzip -1 > '{initrd_path}'",
            shell=True, check=True,
        )
        size_kb = initrd_path.stat().st_size // 1024
        print(f"  [{vm_name}] → {initrd_path.name} ({size_kb} KB)")

    print("build: done")


# ── commands ───────────────────────────────────────────────────────────────────


def cleanup_stale_processes() -> None:
    """Kill lingering console clients and socat processes."""
    subprocess.run(["pkill", "-f", f"socat.*UNIX-CONNECT.*{PONTOON_DIR}"], capture_output=True)
    subprocess.run(["pkill", "-f", f"pontoon.py.*console"], capture_output=True)


def cmd_setup_net(cfg: dict) -> None:
    """One-time host network setup (requires root). Creates bridges and TAPs."""
    setup_network(cfg)
    print("setup-net: bridges and TAPs ready")


def cmd_teardown_net(cfg: dict) -> None:
    """Tear down host bridges and TAPs (requires root)."""
    teardown_network(cfg)
    print("teardown-net: done")


def cmd_up(cfg: dict) -> None:
    # Idempotency: if VMs are already running, stop them first
    state = load_state()
    already_running = any(pid_alive(p) for p in state.get("pids", {}).values())
    if already_running:
        print("up: VMs already running, stopping first...")
        cmd_down(cfg)

    # Clean up stale console/watch processes that might lock the sockets
    cleanup_stale_processes()

    shutil.rmtree(PONTOON_DIR, ignore_errors=True)
    os.makedirs(PONTOON_DIR, exist_ok=True)

    print("up: launching VMs...")
    pids = launch_vms(cfg)
    save_state({"pids": pids})

    print(f"up: waiting for {len(pids)} VMs to boot...")
    try:
        wait_ready(list(pids.keys()), timeout=120)
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)

    print("up: all VMs ready")


def cmd_down(cfg: dict) -> None:
    state = load_state()

    for vm, pid in state.get("pids", {}).items():
        print(f"down: stopping {vm} (pid={pid})...")
        kill_gracefully(pid)

    # Safety net: kill stray QEMU processes that use our TAP names
    for vm_name, svc in cfg["services"].items():
        for i in range(len(svc.get("networks", {}))):
            tap = tap_name(vm_name, i)
            subprocess.run(["pkill", "-f", tap], capture_output=True)

    # Clean up runtime files (bridges/TAPs are persistent — use teardown-net to remove)
    if os.path.exists(PONTOON_DIR):
        shutil.rmtree(PONTOON_DIR, ignore_errors=True)

    print("down: all VMs stopped")


def cmd_status(cfg: dict) -> None:
    state = load_state()
    pids = state.get("pids", {})

    if not pids and not any(iface_exists(f"br-{n}") for n in cfg["networks"]):
        print("status: range is down")
        return

    print("status:")
    for vm_name in cfg["services"]:
        pid = pids.get(vm_name)
        if pid and pid_alive(pid):
            sock = console_sock(vm_name)
            sock_status = "console=ok" if os.path.exists(sock) else "console=missing"
            print(f"  [{vm_name}] running (pid={pid}, {sock_status})")
        elif pid:
            print(f"  [{vm_name}] dead (pid={pid} no longer alive)")
        else:
            print(f"  [{vm_name}] unknown (not in state)")

    for net_name in cfg["networks"]:
        br = f"br-{net_name}"
        status = "up" if iface_exists(br) else "missing"
        print(f"  [network/{net_name}] bridge={br} ({status})")


def cmd_logs(cfg: dict, vm_name: str, lines: int = 50) -> None:
    log_path = vm_log(vm_name)
    if not os.path.exists(log_path):
        print(f"no log found for {vm_name} at {log_path}", file=sys.stderr)
        return
    with open(log_path) as f:
        all_lines = f.readlines()
    for line in all_lines[-lines:]:
        print(line, end="")


def cmd_tcpdump(cfg: dict, network: str, filters: list[str]) -> None:
    br = f"br-{network}"
    if not iface_exists(br):
        print(f"error: bridge {br} not found", file=sys.stderr)
        sys.exit(1)

    cmd = ["sudo", "tcpdump", "-i", br, "-n"] + (filters or [])
    print(f"tcpdump: running {' '.join(cmd)}...")
    os.execvp("sudo", cmd)


def cmd_watch(cfg: dict) -> None:
    if os.geteuid() != 0:
        print("hint: run with sudo for tcpdump visibility", file=sys.stderr)

    if subprocess.run(["which", "tmux"], capture_output=True).returncode != 0:
        print("error: tmux not installed", file=sys.stderr)
        sys.exit(1)

    session = "pontoon"

    # If session exists, attach to it
    if subprocess.run(["tmux", "has-session", "-t", session], capture_output=True).returncode == 0:
        print("watch: attaching to existing dashboard (Ctrl-B d to detach)...")
        subprocess.run(["tmux", "attach-session", "-t", session])
        return

    print("watch: creating new dashboard (Ctrl-B d to detach)...")

    # Clean up any existing socat connections and our own console clients
    cleanup_stale_processes()

    vms = list(cfg["services"].keys())
    networks = list(cfg["networks"].keys())
    
    # We use our own console command instead of socat
    def interactive_cmd(vm):
        # Using sys.executable to ensure we use the same python
        # Robustly resolve script path using realpath to handle symlinks/relative paths
        script_path = os.path.realpath(__file__)
        if not os.path.exists(script_path):
             # Fallback to sys.argv[0] if __file__ is somehow invalid
             script_path = os.path.realpath(sys.argv[0])
             
        if not os.path.exists(script_path):
             print(f"Error: could not locate pontoon.py at {script_path}", file=sys.stderr)
             sys.exit(1)

        return f"while true; do echo 'Connecting to {vm}...'; {sys.executable} {script_path} console {vm}; echo 'Disconnected. Reconnecting in 1s...'; sleep 1; done"

    first_vm = vms[0]
    # tmux commands need to be shell escaped or passed correctly.
    # We pass the full shell command as one argument.
    subprocess.run([
        "tmux", "new-session", "-d", "-s", session,
        "-x", "240", "-y", "50",
        "bash", "-c", interactive_cmd(first_vm),
    ])
    subprocess.run(["tmux", "rename-window", "-t", f"{session}:0", first_vm])

    for vm in vms[1:]:
        subprocess.run([
            "tmux", "split-window", "-t", f"{session}:0", "-h",
            "bash", "-c", interactive_cmd(vm),
        ])

    subprocess.run(["tmux", "select-layout", "-t", f"{session}:0", "even-horizontal"])

    first_net = networks[0]
    subprocess.run(["tmux", "select-pane", "-t", f"{session}:0.0"])
    subprocess.run([
        "tmux", "split-window", "-t", f"{session}:0.0", "-v",
        "tcpdump", "-i", f"br-{first_net}", "-n", "-l",
    ])

    for net in networks[1:]:
        subprocess.run([
            "tmux", "split-window", "-t", f"{session}:0", "-h",
            "tcpdump", "-i", f"br-{net}", "-n", "-l",
        ])

    # Add MCP Audit Log pane
    audit_log = os.path.join(PONTOON_DIR, "mcp-audit.log")
    # Ensure file exists
    if not os.path.exists(audit_log):
        with open(audit_log, "w") as f:
            f.write("--- MCP Audit Log ---\n")

    subprocess.run([
        "tmux", "split-window", "-t", f"{session}:0", "-v",
        "tail", "-f", audit_log,
    ])

    subprocess.run(["tmux", "select-layout", "-t", f"{session}:0", "tiled"])
    subprocess.run(["tmux", "select-pane", "-t", f"{session}:0.0"])
    subprocess.run(["tmux", "attach-session", "-t", session])


def cmd_console(vm_name: str) -> None:
    """Interactive console client. Replaces socat."""
    import termios
    import tty
    import select
    
    sock_path = console_sock(vm_name)
    if not os.path.exists(sock_path):
        print(f"console: socket not found for {vm_name}", file=sys.stderr)
        sys.exit(1)

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.settimeout(5.0) # Fail fast if socket is blocked
        s.connect(sock_path)
        s.settimeout(None) # Back to blocking/default for select
    except Exception as e:
        print(f"console: failed to connect: {e}", file=sys.stderr)
        sys.exit(1)

    print(f"Connected to {vm_name}. Ctrl-] to exit.", flush=True)
    
    # Save terminal settings
    fd = sys.stdin.fileno()
    old_tty = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        
        # Initialize: send a newline to prompt
        s.sendall(b"\n")
        
        # We need to handle:
        # 1. Stdin -> socket
        # 2. Socket -> stdout
        
        while True:
            r, _, _ = select.select([s, fd], [], [])
            
            if s in r:
                try:
                    data = s.recv(4096)
                    if not data:
                        break # EOF from socket
                    os.write(sys.stdout.fileno(), data)
                except OSError:
                    break

            if fd in r:
                try:
                    # Use os.read for unbuffered raw read from stdin
                    data = os.read(fd, 1024)
                    if not data:
                        break # EOF from stdin
                    
                    # Check for escape sequence (Ctrl-])
                    if b'\x1d' in data:
                         break
                         
                    s.sendall(data)
                except OSError:
                    break
                    
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old_tty)
        s.close()
        print("\nconsole: disconnected")


# ── MCP server ─────────────────────────────────────────────────────────────────


TOOLS: dict[str, dict] = {
    "topology_get": {
        "description": "Return parsed compose.yaml topology as JSON",
        "inputSchema": {"type": "object", "properties": {}},
    },
    "range_up": {
        "description": "Start all VMs and wait for them to be ready (bridges must already exist via setup-net)",
        "inputSchema": {"type": "object", "properties": {}},
    },
    "range_down": {
        "description": "Stop all VMs (leaves bridges/TAPs intact)",
        "inputSchema": {"type": "object", "properties": {}},
    },
    "range_status": {
        "description": "Show status of all VMs",
        "inputSchema": {"type": "object", "properties": {}},
    },
    "vm_exec": {
        "description": "Run a command in a VM via serial console",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string", "description": "VM name"},
                "command": {"type": "string", "description": "Shell command to run"},
                "timeout_s": {"type": "integer", "default": 10},
            },
            "required": ["vm", "command"],
        },
    },
    "vm_exec_bg": {
        "description": "Run a background command in a VM, logging to /tmp/<log_tag>.log",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string"},
                "command": {"type": "string"},
                "log_tag": {"type": "string"},
            },
            "required": ["vm", "command", "log_tag"],
        },
    },
    "vm_tail": {
        "description": "Read last N lines of a background command's log",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string"},
                "log_tag": {"type": "string"},
                "lines": {"type": "integer", "default": 50},
            },
            "required": ["vm", "log_tag"],
        },
    },
    "vm_inject": {
        "description": "Copy a file from host into VM via 9p share",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string"},
                "host_path": {"type": "string"},
                "vm_path": {"type": "string"},
            },
            "required": ["vm", "host_path", "vm_path"],
        },
    },
    "vm_logs": {
        "description": "Read last N lines of VM console log (host-side)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string"},
                "lines": {"type": "integer", "default": 50},
            },
            "required": ["vm"],
        },
    },
    "vm_console_stream": {
        "description": "Read raw VM serial output for N seconds",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string"},
                "duration_s": {"type": "integer", "default": 5},
            },
            "required": ["vm"],
        },
    },
    "vm_tcpdump": {
        "description": "Capture traffic on a host bridge for a network zone",
        "inputSchema": {
            "type": "object",
            "properties": {
                "network": {
                    "type": "string",
                    "description": "Network name (e.g. 'perimeter', 'office')",
                },
                "duration_s": {"type": "integer", "default": 5},
                "filter": {"type": "string", "default": ""},
            },
            "required": ["network"],
        },
    },
    "vm_cp": {
        "description": "Copy a host file into a VM via the 9p share",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string", "description": "VM name"},
                "src": {"type": "string", "description": "Host file path to copy"},
                "dst": {"type": "string", "description": "Destination path inside the VM (e.g. /tmp/foo)"},
            },
            "required": ["vm", "src", "dst"],
        },
    },
    "vm_bulk_exec": {
        "description": "Run the same command on multiple VMs in parallel",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vms": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of VM names",
                },
                "command": {"type": "string", "description": "Shell command to run"},
                "timeout_s": {"type": "integer", "default": 10},
            },
            "required": ["vms", "command"],
        },
    },
    "vm_port_probe": {
        "description": "Test TCP connectivity from a VM to a host:port",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string", "description": "VM name"},
                "host": {"type": "string", "description": "Target hostname or IP"},
                "port": {"type": "integer", "description": "Target TCP port"},
                "timeout_s": {"type": "integer", "default": 3},
            },
            "required": ["vm", "host", "port"],
        },
    },
    "vm_ps": {
        "description": "List running processes on a VM",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string", "description": "VM name"},
            },
            "required": ["vm"],
        },
    },
    "vm_pkill": {
        "description": "Send a signal to processes matching a pattern on a VM",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vm": {"type": "string", "description": "VM name"},
                "pattern": {"type": "string", "description": "Process name pattern for pgrep"},
                "signal": {"type": "string", "default": "TERM", "description": "Signal name (e.g. TERM, KILL, HUP)"},
            },
            "required": ["vm", "pattern"],
        },
    },
}


def _mcp_text(text: str) -> dict:
    return {"content": [{"type": "text", "text": text}]}


def _capture_output(fn) -> str:
    import io
    buf = io.StringIO()
    old_stdout = sys.stdout
    sys.stdout = buf
    try:
        fn()
    finally:
        sys.stdout = old_stdout
    return buf.getvalue()


def handle_tool_call(name: str, args: dict, cfg: dict) -> dict:
    try:
        if name == "topology_get":
            return _mcp_text(json.dumps(cfg, indent=2))

        elif name == "range_up":
            out = _capture_output(lambda: cmd_up(cfg))
            return _mcp_text(out)

        elif name == "range_down":
            out = _capture_output(lambda: cmd_down(cfg))
            return _mcp_text(out)

        elif name == "range_status":
            out = _capture_output(lambda: cmd_status(cfg))
            return _mcp_text(out)

        elif name == "vm_exec":
            vm = args["vm"]
            command = args["command"]
            timeout_s = args.get("timeout_s", 10)
            result = serial_send_recv(vm, command, timeout_s=timeout_s)
            return _mcp_text(result)

        elif name == "vm_exec_bg":
            vm = args["vm"]
            command = args["command"]
            log_tag = args["log_tag"]
            bg_cmd = f"nohup sh -c {json.dumps(command)} > /tmp/{log_tag}.log 2>&1 &"
            serial_send_recv(vm, bg_cmd, timeout_s=5)
            return _mcp_text(f"started background command, logging to /tmp/{log_tag}.log")

        elif name == "vm_tail":
            vm = args["vm"]
            log_tag = args["log_tag"]
            lines = args.get("lines", 50)
            result = serial_send_recv(vm, f"tail -n {lines} /tmp/{log_tag}.log", timeout_s=10)
            return _mcp_text(result)

        elif name == "vm_inject":
            vm = args["vm"]
            host_path = args["host_path"]
            vm_path = args["vm_path"]
            share_dir = f"{PONTOON_DIR}/share/{vm}"
            os.makedirs(share_dir, exist_ok=True)
            dest_name = os.path.basename(vm_path)
            host_dest = os.path.join(share_dir, dest_name)
            shutil.copy2(host_path, host_dest)
            # Move into place in VM
            serial_send_recv(vm, f"cp /mnt/share/{vm}/{dest_name} {vm_path}", timeout_s=5)
            return _mcp_text(f"injected {host_path} → {vm}:{vm_path}")

        elif name == "vm_logs":
            vm = args["vm"]
            lines = int(args.get("lines", 50))
            log_path = vm_log(vm)
            if not os.path.exists(log_path):
                return _mcp_text(f"no log found at {log_path}")
            with open(log_path) as f:
                all_lines = f.readlines()
            return _mcp_text("".join(all_lines[-lines:]))

        elif name == "vm_console_stream":
            vm = args["vm"]
            duration_s = args.get("duration_s", 5)
            result = serial_read_duration(vm, duration_s)
            return _mcp_text(result)

        elif name == "vm_tcpdump":
            network = args["network"]
            duration_s = args.get("duration_s", 5)
            filt = args.get("filter", "")
            br = f"br-{network}"
            cmd = ["sudo", "tcpdump", "-i", br, "-n", "-c", "200"]
            if filt:
                cmd += filt.split()
            try:
                result = subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    timeout=duration_s + 2,
                )
                return _mcp_text(result.stdout + result.stderr)
            except subprocess.TimeoutExpired as e:
                output = (e.stdout or b"").decode(errors="replace")
                return _mcp_text(output or "(no packets captured)")

        elif name == "vm_cp":
            vm = args["vm"]
            src = args["src"]
            dst = args["dst"]
            share_dir = f"{PONTOON_DIR}/share/{vm}"
            os.makedirs(share_dir, exist_ok=True)
            basename = os.path.basename(src)
            host_dest = os.path.join(share_dir, basename)
            shutil.copy2(src, host_dest)
            is_exec = os.access(src, os.X_OK)
            try:
                cp_cmd = f"cp /mnt/share/{basename} {dst}"
                if is_exec:
                    cp_cmd += f" && chmod +x {dst}"
                serial_send_recv(vm, cp_cmd, timeout_s=10)
            finally:
                try:
                    os.remove(host_dest)
                except OSError:
                    pass
            return _mcp_text(f"copied {src} → {vm}:{dst}")

        elif name == "vm_bulk_exec":
            vms = args["vms"]
            command = args["command"]
            timeout_s = args.get("timeout_s", 10)
            import concurrent.futures
            results: dict[str, str] = {}

            def _exec_one(vm_name: str) -> tuple[str, str]:
                try:
                    out = serial_send_recv(vm_name, command, timeout_s=timeout_s)
                    return vm_name, out.strip()
                except Exception as exc:
                    return vm_name, f"error: {exc}"

            with concurrent.futures.ThreadPoolExecutor(max_workers=len(vms)) as pool:
                futures = {pool.submit(_exec_one, vm): vm for vm in vms}
                for fut in concurrent.futures.as_completed(futures):
                    vm_name, out = fut.result()
                    results[vm_name] = out
            return _mcp_text(json.dumps(results, indent=2))

        elif name == "vm_port_probe":
            vm = args["vm"]
            host = args["host"]
            port = int(args["port"])
            timeout_s = args.get("timeout_s", 3)
            probe_cmd = f"nc -w{timeout_s} {host} {port} </dev/null 2>/dev/null; echo $?"
            result = serial_send_recv(vm, probe_cmd, timeout_s=timeout_s + 3)
            exit_code = result.strip().splitlines()[-1] if result.strip() else "1"
            open_flag = exit_code.strip() == "0"
            return _mcp_text(json.dumps({"open": open_flag, "host": host, "port": port}))

        elif name == "vm_ps":
            vm = args["vm"]
            result = serial_send_recv(vm, "ps -o pid,comm,args 2>/dev/null || ps aux", timeout_s=10)
            processes = []
            lines = result.strip().splitlines()
            # Skip header line
            for line in lines[1:]:
                line = line.strip()
                if not line:
                    continue
                parts = line.split(None, 2)
                if len(parts) >= 2:
                    try:
                        pid = int(parts[0])
                    except ValueError:
                        continue
                    name_field = parts[1] if len(parts) > 1 else ""
                    args_field = parts[2] if len(parts) > 2 else ""
                    processes.append({"pid": pid, "name": name_field, "args": args_field})
            return _mcp_text(json.dumps(processes, indent=2))

        elif name == "vm_pkill":
            vm = args["vm"]
            pattern = args["pattern"]
            sig = args.get("signal", "TERM")
            kill_cmd = f"kill -{sig} $(pgrep {pattern}) 2>/dev/null; echo $?"
            result = serial_send_recv(vm, kill_cmd, timeout_s=10)
            exit_code = result.strip().splitlines()[-1] if result.strip() else "1"
            success = exit_code.strip() == "0"
            return _mcp_text(json.dumps({"success": success, "pattern": pattern, "signal": sig}))

        else:
            return {"content": [{"type": "text", "text": f"unknown tool: {name}"}], "isError": True}

    except Exception as e:
        return {"content": [{"type": "text", "text": f"error: {e}"}], "isError": True}


def serve_mcp(cfg: dict) -> None:
    """stdio MCP server — JSON-RPC 2.0."""

    def send(obj: dict) -> None:
        line = json.dumps(obj)
        sys.stdout.write(line + "\n")
        sys.stdout.flush()

    def make_tools_list() -> list:
        return [
            {"name": name, "description": spec["description"], "inputSchema": spec["inputSchema"]}
            for name, spec in TOOLS.items()
        ]

    for raw_line in sys.stdin:
        raw_line = raw_line.strip()
        if not raw_line:
            continue
        try:
            req = json.loads(raw_line)
        except json.JSONDecodeError:
            continue

        method = req.get("method", "")
        req_id = req.get("id")
        params = req.get("params", {})

        if method == "initialize":
            send({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "pontoon", "version": "0.1.0"},
                },
            })

        elif method == "initialized":
            # Notification — no response
            pass

        elif method == "tools/list":
            send({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {"tools": make_tools_list()},
            })

        elif method == "tools/call":
            tool_name = params.get("name", "")
            tool_args = params.get("arguments", {})
            result = handle_tool_call(tool_name, tool_args, cfg)
            send({"jsonrpc": "2.0", "id": req_id, "result": result})

        elif req_id is not None:
            send({
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {"code": -32601, "message": f"method not found: {method}"},
            })


# ── main ───────────────────────────────────────────────────────────────────────


def main() -> None:
    here = Path(__file__).parent
    compose_path = str(here / "pontoon.yml")

    ap = argparse.ArgumentParser(description="Pontoon — QEMU microVM range orchestrator")
    ap.add_argument(
        "--compose", default=compose_path, metavar="FILE",
        help=f"path to pontoon.yml (default: {compose_path})"
    )
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("build", help="Build per-VM initrds from layer definitions")
    sub.add_parser("setup-net", help="One-time host network setup (requires root)")
    sub.add_parser("teardown-net", help="Remove host bridges and TAPs (requires root)")
    sub.add_parser("up", help="Start all VMs (no root needed)")
    sub.add_parser("down", help="Stop all VMs (no root needed)")
    sub.add_parser("status", help="Show VM status")

    p_logs = sub.add_parser("logs", help="Show VM console log")
    p_logs.add_argument("vm", help="VM name")
    p_logs.add_argument("--lines", type=int, default=50)

    p_tcpdump = sub.add_parser("tcpdump", help="Run tcpdump on a network bridge")
    p_tcpdump.add_argument("network", help="Network name (e.g. perimeter)")
    p_tcpdump.add_argument("filter", nargs=argparse.REMAINDER, help="tcpdump filters")

    p_exec = sub.add_parser("exec", help="Execute command in VM via MCP channel")
    p_exec.add_argument("vm", help="VM name")
    p_exec.add_argument("command", help="Command to run")
    p_exec.add_argument("--timeout", type=int, default=10)

    sub.add_parser("watch", help="Open tmux dashboard")
    p_console = sub.add_parser("console", help="Interactive console client (internal)")
    p_console.add_argument("vm", help="VM name")
    sub.add_parser("mcp", help="Start MCP stdio server")

    args = ap.parse_args()
    cfg = parse_compose(args.compose)

    if args.cmd == "build":
        cmd_build(cfg, Path(args.compose).parent)
    elif args.cmd == "setup-net":
        cmd_setup_net(cfg)
    elif args.cmd == "teardown-net":
        cmd_teardown_net(cfg)
    elif args.cmd == "up":
        cmd_up(cfg)
    elif args.cmd == "down":
        cmd_down(cfg)
    elif args.cmd == "status":
        cmd_status(cfg)
    elif args.cmd == "logs":
        cmd_logs(cfg, args.vm, args.lines)
    elif args.cmd == "tcpdump":
        cmd_tcpdump(cfg, args.network, args.filter)
    elif args.cmd == "exec":
        try:
            print(serial_send_recv(args.vm, args.command, timeout_s=args.timeout))
        except Exception as e:
            print(f"error: {e}", file=sys.stderr)
            sys.exit(1)
    elif args.cmd == "watch":
        cmd_watch(cfg)
    elif args.cmd == "console":
        cmd_console(args.vm)
    elif args.cmd == "mcp":
        serve_mcp(cfg)


if __name__ == "__main__":
    main()
