set shell := ["bash", "-euo", "pipefail", "-c"]

root := source_directory() / ".."
dir := source_directory()
vm_dir := dir / "vm"
build_dir := vm_dir / "build"

default: status

# Build range-specific kernel with vsock support (one-time, cached)
build-kernel:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f "{{ build_dir }}/vmlinuz" ]]; then echo "using cached kernel"; exit 0; fi
    mkdir -p "{{ build_dir }}"
    docker build \
        -f {{ vm_dir }}/kernel/Dockerfile \
        --output type=local,dest={{ build_dir }} \
        {{ vm_dir }}/kernel/

# Build vsock-enabled wallhack musl binary
build-wallhack:
    CARGO_TARGET_DIR={{ root }}/target/musl cross build --release \
        --target x86_64-unknown-linux-musl \
        --manifest-path {{ root }}/crates/cli/Cargo.toml \
        --no-default-features --features "slim,vsock"

# Build per-VM initrds from layer definitions in range/layers/
build-layers:
    python3 {{ dir }}/pontoon.py build

# List initrd contents for a VM (debug)
check-initrd vm='attacker':
    zcat "{{ build_dir }}/initrd-{{ vm }}.gz" | cpio -tv --quiet 2>/dev/null | sort

# Full build: wallhack binary + all VM initrds
build: build-wallhack build-layers

# One-time host network setup — creates bridges and TAPs (requires root)
setup-net:
    sudo python3 {{ dir }}/pontoon.py setup-net

# Remove host bridges and TAPs (requires root)
teardown-net:
    sudo python3 {{ dir }}/pontoon.py teardown-net

# Start the range
up:
    python3 {{ dir }}/pontoon.py up

# Stop and clean up the range
down:
    python3 {{ dir }}/pontoon.py down

# Show VM status
status:
    python3 {{ dir }}/pontoon.py status

# Tail VM console log (host-side)
logs vm='attacker':
    python3 {{ dir }}/pontoon.py logs {{ vm }}

# Follow VM log in real time
tail vm='attacker':
    tail -f /tmp/pontoon/{{ vm }}.log

# Connect to VM serial console (interactive)
shell vm='attacker':
    socat STDIO UNIX-CONNECT:/tmp/pontoon/{{ vm }}-console.sock

# Open live tmux dashboard (VM logs + tcpdump per zone)
watch:
    python3 {{ dir }}/pontoon.py watch

# Start MCP stdio server (agent connects to this)
mcp:
    python3 {{ dir }}/pontoon.py mcp
