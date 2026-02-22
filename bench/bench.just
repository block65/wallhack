iperf3_ver := "3.20"
kernel_ver := "6.19"
busybox_ver := "1.37.0"
socat_ver := "1.8.1.1"
musl_target := "x86_64-unknown-linux-musl"
vm_dir := "vm"
staging_dir := vm_dir / "staging"
rootfs_dir := staging_dir / "rootfs"
tools_dir := staging_dir / "bin"
vmlinuz := staging_dir / "vmlinuz"
initrd := staging_dir / "initrd.gz"
wallhack_bin := justfile_directory() / "target" / musl_target / "release" / "wallhack"
busybox_bin := tools_dir / "busybox"
iperf3_bin := tools_dir / "iperf3"
socat_bin := tools_dir / "socat"

# Build minimal kernel with TUN + virtio-net built-in (cached after first build).

# Also builds static socat binary used in the initrd.
build-kernel:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f "{{ vmlinuz }}" && -f "{{ socat_bin }}" ]]; then
        echo "using cached kernel and socat"
        exit 0
    fi
    mkdir -p "{{ staging_dir }}" "{{ tools_dir }}"
    echo "Building kernel {{ kernel_ver }} ¯\_(ツ)_/¯"
    DOCKER_BUILDKIT=1 docker build \
        -f "{{ vm_dir }}/kernel/Dockerfile" \
        --progress=plain \
        --build-arg KERNEL_VERSION="{{ kernel_ver }}" \
        --build-arg SOCAT_VERSION="{{ socat_ver }}" \
        --output "type=local,dest={{ staging_dir }}" \
        "{{ vm_dir }}/kernel/"
    echo "kernel and socat built"

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

cargo-build-musl:
    echo "Building musl binary (slim)..."
    cross build --release --target "{{ musl_target }}" --features slim

clean:
    rm -rf "{{ staging_dir }}"

# Build initramfs from musl wallhack + busybox + socat + iperf3 + init.sh
build-initrd: cargo-build-musl build-kernel fetch-busybox fetch-iperf3
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
    cp "{{ vm_dir }}/init.sh" "{{ rootfs_dir }}/init"
    chmod +x "{{ rootfs_dir }}/init"

    echo "creating initrd..."
    (cd "{{ rootfs_dir }}" && find . | cpio -o -H newc --quiet) | gzip > "{{ initrd }}"
    echo "done"

# VM integration tests: basic tunnel connectivity (QUIC + WebSocket)
smoke: build-initrd
    python3 run_tests.py smoke

# VM integration tests: degraded network resilience (QUIC + WebSocket)
resilience: build-initrd
    python3 run_tests.py resilience

# VM integration benchmarks: throughput + latency (not in `just check`)
benchmark: build-initrd
    python3 run_benchmarks.py

# Boot both VMs interactively for manual topology inspection
debug-topology: build-initrd
    python3 run_tests.py debug-topology
