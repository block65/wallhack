FROM debian:trixie-slim AS builder

RUN apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends \
    ca-certificates curl xz-utils \
    build-essential \
    bc flex bison \
    libssl-dev libelf-dev && \
    rm -rf /var/lib/apt/lists/*

ARG KERNEL_VERSION=6.19

WORKDIR /build/linux-${KERNEL_VERSION}
RUN KERNEL_MAJOR=$(echo ${KERNEL_VERSION} | cut -d. -f1) && \
    curl -fsSL "https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_MAJOR}.x/linux-${KERNEL_VERSION}.tar.xz" \
    | tar -xJ --strip-components=1

RUN make tinyconfig

RUN ./scripts/config \
    -e 64BIT \
    -e BINFMT_ELF \
    -e BINFMT_SCRIPT \
    `# --- Ramdisk Support ---` \
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
    -e SERIAL_8250_NR_UARTS=4 \
    -e SERIAL_8250_RUNTIME_UARTS=4 \
    -e MAGIC_SYSRQ \
    `# --- Filesystems ---` \
    -e PROC_FS \
    -e SYSFS \
    -e DEVTMPFS \
    -e DEVTMPFS_MOUNT \
    -e TMPFS \
    `# --- Syscalls needed by Tokio ---` \
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
    `# --- Traffic Control ---` \
    -e NET_SCHED \
    -e NET_SCH_NETEM \
    `# --- Virtio MMIO Subsystem ---` \
    -e VIRTIO_MENU \
    -e VIRTIO \
    -e VIRTIO_CONSOLE \
    -e VIRTIO_MMIO \
    -e VIRTIO_MMIO_CMDLINE_DEVICES \
    -e VIRTIO_NET \
    `# --- vsock (guest<->host IPC) ---` \
    -e VSOCKETS \
    -e VIRTIO_VSOCKETS \
    `# --- 9p (host share for vm_cp) ---` \
    -e NET_9P \
    -e NET_9P_VIRTIO \
    -e 9P_FS \
    `# --- Entropy ---` \
    -e HW_RANDOM \
    `# --- Multi-user support (needed for setgroups, used by postgres/mariadb/sockd) ---` \
    -e EXPERT \
    -e MULTIUSER \
    `# --- SysV IPC (needed by postgres shared memory: shmget) ---` \
    -e SYSVIPC \
    `# --- Async IO (needed by mariadb/InnoDB: io_setup) ---` \
    -e AIO \
    `# --- File locking (needed by prometheus, mariadb: flock/fcntl) ---` \
    -e FILE_LOCKING \
    -d MODULES

RUN make olddefconfig

RUN echo "=== CONFIG CHECK ===" && \
    grep -E "^CONFIG_VSOCKETS=|^CONFIG_VIRTIO_VSOCKETS=|^CONFIG_TUN=|^CONFIG_VIRTIO" .config

RUN grep "CONFIG_TUN=y"              .config || (echo "FAIL: CONFIG_TUN not built-in"              && exit 1)
RUN grep "CONFIG_VIRTIO_NET=y"       .config || (echo "FAIL: CONFIG_VIRTIO_NET not built-in"       && exit 1)
RUN grep "CONFIG_VIRTIO_MMIO=y"      .config || (echo "FAIL: CONFIG_VIRTIO_MMIO not built-in"      && exit 1)
RUN grep "CONFIG_NET_SCH_NETEM=y"    .config || (echo "FAIL: CONFIG_NET_SCH_NETEM not built-in"    && exit 1)
RUN grep "CONFIG_VSOCKETS=y"         .config || (echo "FAIL: CONFIG_VSOCKETS not built-in"         && exit 1)
RUN grep "CONFIG_VIRTIO_VSOCKETS=y"  .config || (echo "FAIL: CONFIG_VIRTIO_VSOCKETS not built-in"  && exit 1)
RUN ! grep "CONFIG_MODULES=y"        .config || (echo "FAIL: CONFIG_MODULES is enabled"            && exit 1)
RUN ! grep "CONFIG_PCI=y"            .config || (echo "FAIL: CONFIG_PCI is enabled"                && exit 1)

RUN make -j"$(nproc)" bzImage
RUN cp arch/x86/boot/bzImage /build/vmlinuz

FROM scratch
ARG KERNEL_VERSION=6.19
COPY --from=builder /build/vmlinuz /vmlinuz
COPY --from=builder /build/linux-${KERNEL_VERSION}/.config /kernel.config
