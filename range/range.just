set shell := ["bash", "-euo", "pipefail", "-c"]

COMPOSE := "range/pontoon.yml"
WALLHACK_BINARY := "../target/musl/x86_64-unknown-linux-musl/release/wallhack"

# Build kernel image into range/vm/build/vmlinuz (one-time, cached)
build-kernel:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p range/vm/build
    if [[ -f "range/vm/build/vmlinuz" ]]; then
        echo "using cached kernel"
        exit 0
    fi
    docker build \
        -f ../pontoon/vm/kernel/Dockerfile \
        --output type=local,dest=range/vm/build \
        ../pontoon/vm/kernel/

# Build per-VM initrds
build:
    pontoon --compose {{ COMPOSE }} build --set WALLHACK_BINARY={{ WALLHACK_BINARY }}

# One-time host network setup (requires root)
setup-net:
    sudo pontoon --compose {{ COMPOSE }} setup-net

# Remove host bridges and TAPs (requires root)
teardown-net:
    sudo pontoon --compose {{ COMPOSE }} teardown-net

# Start all VMs
up:
    pontoon --compose {{ COMPOSE }} up

# Stop all VMs
down:
    pontoon --compose {{ COMPOSE }} down

# Show VM status
status:
    pontoon --compose {{ COMPOSE }} status

# Tail VM console log
logs vm='attacker':
    pontoon --compose {{ COMPOSE }} logs {{ vm }}

# Follow VM log in real time
tail vm='attacker':
    tail -f /tmp/pontoon/{{ vm }}.log

# Interactive serial console
shell vm='attacker':
    pontoon --compose {{ COMPOSE }} console {{ vm }}

# Open live tmux dashboard
watch:
    pontoon --compose {{ COMPOSE }} watch

# Start MCP stdio server
mcp:
    pontoon --compose {{ COMPOSE }} mcp

# List initrd contents for a VM
check-initrd vm='attacker':
    zcat "range/vm/build/initrd-{{ vm }}.gz" | cpio -tv --quiet 2>/dev/null | sort
