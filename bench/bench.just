iperf3_ver := "3.20"
kernel_ver := "6.19"
busybox_ver := "1.37.0"
socat_ver := "1.8.1.1"
iproute2_ver := "6.12.0"
musl_target := "x86_64-unknown-linux-musl"
musl_target_dir := "target/musl"
vm_dir := "vm"
staging_dir := vm_dir / "staging"
rootfs_dir := staging_dir / "rootfs"
tools_dir := staging_dir / "bin"
vmlinuz := staging_dir / "vmlinuz"
initrd_base := staging_dir / "initrd-base.gz"
initrd_overlay := staging_dir / "initrd-overlay.gz"
initrd := staging_dir / "initrd.gz"
wallhack_bin := justfile_directory() / musl_target_dir / musl_target / "release" / "wallhack"
busybox_bin := tools_dir / "busybox"
iperf3_bin := tools_dir / "iperf3"
socat_bin := tools_dir / "socat"
tc_bin := tools_dir / "tc"

# Default: run benchmarks
default: benchmark

# Build minimal kernel with TUN + virtio-net built-in (cached after first build).
build-kernel:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f "{{ vmlinuz }}" ]]; then
        echo "using cached kernel"
        exit 0
    fi
    mkdir -p "{{ staging_dir }}" "{{ tools_dir }}"
    echo "Building kernel {{ kernel_ver }} ¯\_(ツ)_/¯"
    DOCKER_BUILDKIT=1 docker build \
        -f "{{ vm_dir }}/kernel/Dockerfile" \
        --progress=plain \
        --build-arg KERNEL_VERSION="{{ kernel_ver }}" \
        --output "type=local,dest={{ staging_dir }}" \
        "{{ vm_dir }}/kernel/"
    echo "kernel built"

# Build static socat binary used in the initrd (cached after first build).
build-socat:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f "{{ socat_bin }}" ]]; then
        echo "using cached socat"
        exit 0
    fi
    mkdir -p "{{ staging_dir }}" "{{ tools_dir }}"
    echo "Building socat {{ socat_ver }}..."
    DOCKER_BUILDKIT=1 docker build \
        -f "{{ vm_dir }}/socat/Dockerfile" \
        --progress=plain \
        --build-arg SOCAT_VERSION="{{ socat_ver }}" \
        --output "type=local,dest={{ staging_dir }}" \
        "{{ vm_dir }}/socat/"
    echo "socat built"

# Download static busybox binary
fetch-busybox:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -x "{{ busybox_bin }}" ]]; then
        echo "using cached busybox at {{ busybox_bin }}"
        exit 0
    fi
    mkdir -p "{{ tools_dir }}"
    url="https://busybox.net/downloads/binaries/{{ busybox_ver }}-x86_64-linux-musl/busybox"
    echo "Downloading busybox {{ busybox_ver }}..."
    curl -fsSL -o "{{ busybox_bin }}" "$url"
    chmod +x "{{ busybox_bin }}"
    echo "busybox downloaded to {{ busybox_bin }}"

# Download static iperf3 binary (for benchmark scenario)
fetch-iperf3:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -x "{{ iperf3_bin }}" ]]; then
        echo "using cached iperf3 at {{ iperf3_bin }}"
        exit 0
    fi
    mkdir -p "{{ tools_dir }}"
    url="https://github.com/userdocs/iperf3-static/releases/download/{{ iperf3_ver }}/iperf3-amd64"
    echo "Downloading iperf3 {{ iperf3_ver }}..."
    curl -fsSL -o "{{ iperf3_bin }}" "$url"
    chmod +x "{{ iperf3_bin }}"
    echo "iperf3 downloaded to {{ iperf3_bin }}"

# Build static tc binary from iproute2 (for netem in benchmark VMs).
build-tc:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f "{{ tc_bin }}" ]]; then
        echo "using cached tc"
        exit 0
    fi
    mkdir -p "{{ staging_dir }}" "{{ tools_dir }}"
    echo "Building tc (iproute2 {{ iproute2_ver }})..."
    DOCKER_BUILDKIT=1 docker build \
        -f "{{ vm_dir }}/iproute2/Dockerfile" \
        --progress=plain \
        --build-arg IPROUTE2_VERSION="{{ iproute2_ver }}" \
        --output "type=local,dest={{ staging_dir }}" \
        "{{ vm_dir }}/iproute2/"
    echo "tc built"

cargo-build-musl:
    echo "Building musl binary (slim)..."
    CARGO_TARGET_DIR={{ musl_target_dir }} cross build --release --target "{{ musl_target }}" -p wallhack-cli --no-default-features --features slim

clean:
    rm -rf "{{ staging_dir }}"

# Build base initramfs: binaries only (slow, but cached until binaries change).
build-initrd-base: cargo-build-musl build-kernel build-socat build-tc fetch-busybox fetch-iperf3
    #!/usr/bin/env bash
    set -euo pipefail

    mkdir -p {{ rootfs_dir }}/{bin,proc,sys,dev,tmp}
    mkdir -p {{ rootfs_dir }}/dev/{pts,net}

    echo "linking busybox applets..."
    "{{ busybox_bin }}" --list | while read -r applet; do
        ln -sf /bin/busybox "{{ rootfs_dir }}/bin/$applet"
    done

    # Bake static binaries directly into the initrd
    cp "{{ wallhack_bin }}" "{{ rootfs_dir }}/bin/wallhack"
    chmod +x "{{ rootfs_dir }}/bin/wallhack"
    cp "{{ busybox_bin }}" "{{ rootfs_dir }}/bin/busybox"
    chmod +x "{{ rootfs_dir }}/bin/busybox"
    cp "{{ socat_bin }}" "{{ rootfs_dir }}/bin/socat"
    chmod +x "{{ rootfs_dir }}/bin/socat"
    cp "{{ iperf3_bin }}" "{{ rootfs_dir }}/bin/iperf3"
    chmod +x "{{ rootfs_dir }}/bin/iperf3"
    rm -f "{{ rootfs_dir }}/bin/tc"
    cp "{{ tc_bin }}" "{{ rootfs_dir }}/bin/tc"
    chmod +x "{{ rootfs_dir }}/bin/tc"

    # Base initrd: everything except /init (which comes from the overlay)
    rm -f "{{ rootfs_dir }}/init"
    echo "creating base initrd..."
    (cd "{{ rootfs_dir }}" && find . | cpio -o -H newc --quiet) | gzip -1 > "{{ initrd_base }}"
    echo "done"

# Build init overlay: just /init from init.sh (instant).
build-initrd-overlay:
    #!/usr/bin/env bash
    set -euo pipefail
    _tmp=$(mktemp -d)
    cp "{{ vm_dir }}/init.sh" "${_tmp}/init"
    chmod +x "${_tmp}/init"
    (cd "${_tmp}" && echo init | cpio -o -H newc --quiet) | gzip -1 > "{{ initrd_overlay }}"
    rm -rf "${_tmp}"

# Assemble final initrd by concatenating base + overlay.
# The kernel extracts both; overlay's /init overwrites the base.
build-initrd: build-initrd-base build-initrd-overlay
    cat "{{ initrd_base }}" "{{ initrd_overlay }}" > "{{ initrd }}"

# Fast rebuild: only regenerate the init.sh overlay (skips cargo/docker entirely).
rebuild-init: build-initrd-overlay
    cat "{{ initrd_base }}" "{{ initrd_overlay }}" > "{{ initrd }}"

# VM integration tests: basic tunnel connectivity (QUIC + WebSocket)
smoke: build-initrd
    python3 run.py run --tag smoke --duration 2

# VM integration tests: degraded network resilience (QUIC + WebSocket)
resilience: build-initrd
    python3 run.py run --tag resilience --duration 2

# VM integration benchmarks: throughput + latency (not in `just check`)
benchmark: build-initrd
    python3 run.py run --tag benchmark --runs 3

# Boot both VMs interactively for manual topology inspection
debug-topology: build-initrd
    python3 run.py debug-topology

# Boot a single interactive busybox shell VM (Ctrl-A X to exit QEMU)
shell: build-initrd
    python3 run.py debug-shell
