FROM debian:trixie-slim AS builder

RUN apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends \
    ca-certificates curl xz-utils \
    build-essential autoconf automake \
    bc flex bison \
    libssl-dev libelf-dev && \
    rm -rf /var/lib/apt/lists/*

ARG SOCAT_VERSION=1.8.1.1
ARG KERNEL_VERSION

# Static socat
WORKDIR /build/socat-${SOCAT_VERSION}
## socat author doesnt even have SSL, so we get it from the googlesource mirror
RUN curl -fsSL "https://third-party-mirror.googlesource.com/socat/+archive/refs/tags/tag-${SOCAT_VERSION}.tar.gz" | tar -xz 
RUN autoconf && ./configure --disable-shared LDFLAGS="-static" && \
    make -j"$(nproc)" && \
    strip socat && \
    cp socat /build/socat

# Linux Kernel
# WARNING: the hardcoded v6.x prefix means you cant actually download
# anything other than 6.x with this Dockerfile. never mind.
WORKDIR /build/linux-${KERNEL_VERSION}
RUN KERNEL_MAJOR=$(echo ${KERNEL_VERSION} | cut -d. -f1) && \
    curl -fsSL "https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_MAJOR}.x/linux-${KERNEL_VERSION}.tar.xz" \
    | tar -xJ --strip-components=1

RUN make tinyconfig

# enable subsystems AND the top-level gatekeeper menus
RUN ./scripts/config \
    -e 64BIT \
    -e BINFMT_ELF \
    -e BINFMT_SCRIPT \
    `# --- Ramdisk Support (REQUIRED to boot without a hard drive) ---` \
    -e BLK_DEV_INITRD \
    -e RD_GZIP \
    `# --- KVM Paravirtualization Core ---` \
    -e HYPERVISOR_GUEST \
    -e PARAVIRT \
    -e KVM_GUEST \
    -e KVM_CLOCK \
    `# --- Console & TTY ---` \
    -e TTY \
    -e PRINTK \
    -e SERIAL_8250 \
    -e SERIAL_8250_CONSOLE \
    -e MAGIC_SYSRQ \
    `# --- Filesystems (REQUIRED for network configuration) ---` \
    -e PROC_FS \
    -e SYSFS \
    -e DEVTMPFS \
    -e DEVTMPFS_MOUNT \
    -e TMPFS \
    `# --- Syscalls needed by Tokio async runtime ---` \
    -e FUTEX \
    -e EPOLL \
    -e EVENTFD \
    -e TIMERFD \
    -e SIGNALFD \
    `# --- Networking ---` \
    -e NET \
    -e INET \
    -e UNIX \
    -e NETDEVICES \
    -e NET_CORE \
    -e TUN \
    `# --- Traffic Control (REQUIRED for netem resilience) ---` \
    -e NET_SCHED \
    -e NET_SCH_NETEM \
    `# --- Virtio MMIO Subsystem ---` \
    -e VIRTIO_MENU \
    -e VIRTIO \
    -e VIRTIO_MMIO \
    -e VIRTIO_MMIO_CMDLINE_DEVICES \
    -e VIRTIO_NET \
    `# --- Entropy / RNG (RDRAND via -cpu host) ---` \
    -e HW_RANDOM \
    -d MODULES

# dependencies
RUN make olddefconfig

# config checks
RUN echo "=== CONFIG CHECK ===" && \
    grep -E "^CONFIG_MODULES=|^CONFIG_TUN=|^CONFIG_VIRTIO|^CONFIG_PCI=" .config

RUN grep "CONFIG_TUN=y"           .config || (echo "FAIL: CONFIG_TUN not built-in"           && exit 1)
RUN grep "CONFIG_VIRTIO_NET=y"    .config || (echo "FAIL: CONFIG_VIRTIO_NET not built-in"    && exit 1)
RUN grep "CONFIG_VIRTIO_MMIO=y"   .config || (echo "FAIL: CONFIG_VIRTIO_MMIO not built-in"   && exit 1)
RUN grep "CONFIG_NET_SCH_NETEM=y" .config || (echo "FAIL: CONFIG_NET_SCH_NETEM not built-in" && exit 1)
RUN ! grep "CONFIG_MODULES=y"     .config || (echo "FAIL: CONFIG_MODULES is enabled"         && exit 1)
RUN ! grep "CONFIG_PCI=y"         .config || (echo "FAIL: CONFIG_PCI is enabled"             && exit 1)

# build
RUN make -j"$(nproc)" bzImage
RUN cp arch/x86/boot/bzImage /build/vmlinuz

FROM scratch
COPY --from=builder /build/vmlinuz /vmlinuz
COPY --from=builder /build/socat /bin/socat
