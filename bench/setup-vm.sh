#!/usr/bin/env bash
# bench/setup-vm.sh — build bench/vm/base.qcow2
#
# Downloads Ubuntu 22.04 minimal cloud image, uses cloud-init to install
# required packages and write the wallhack-init script, boots the VM once
# to apply cloud-init, then saves the result as bench/vm/base.qcow2.
#
# Requires: qemu-system-x86_64, qemu-img, cloud-image-utils (cloud-localds)
#           OR genisoimage/mkisofs (alternative seed ISO creation)
#
# Usage: bash bench/setup-vm.sh

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "${SCRIPT_DIR}/.." && pwd)
VM_DIR="${SCRIPT_DIR}/vm"
BASE_IMAGE="${VM_DIR}/base.qcow2"
INIT_SCRIPT="${VM_DIR}/init.sh"

# Ubuntu 22.04 LTS minimal cloud image (amd64)
CLOUD_IMAGE_URL="https://cloud-images.ubuntu.com/minimal/releases/jammy/release/ubuntu-22.04-minimal-cloudimg-amd64.img"
CLOUD_IMAGE="${VM_DIR}/ubuntu-22.04-minimal-cloudimg-amd64.img"
WORK_IMAGE="${VM_DIR}/setup-work.qcow2"
SEED_ISO="${VM_DIR}/cloud-seed.iso"

# ── preflight checks ──────────────────────────────────────────────────────────
check_deps() {
    local missing=()
    command -v qemu-system-x86_64 >/dev/null || missing+=("qemu-system-x86_64")
    command -v qemu-img            >/dev/null || missing+=("qemu-img")
    if ! command -v cloud-localds >/dev/null && \
       ! command -v genisoimage   >/dev/null && \
       ! command -v mkisofs       >/dev/null; then
        missing+=("cloud-localds (cloud-image-utils) or genisoimage/mkisofs")
    fi
    if [ ${#missing[@]} -gt 0 ]; then
        echo "Missing required tools:"
        for t in "${missing[@]}"; do echo "  $t"; done
        echo ""
        echo "Install on Ubuntu: sudo apt-get install -y qemu-system-x86 qemu-utils cloud-image-utils"
        exit 1
    fi
    if ! [ -r /dev/kvm ]; then
        echo "Warning: /dev/kvm not accessible. VM setup will be very slow without KVM."
        echo "Add yourself to the kvm group: sudo usermod -aG kvm \$USER"
        echo "Continuing anyway..."
        KVM_FLAGS=""
    else
        KVM_FLAGS="-enable-kvm"
    fi
}

# ── download cloud image ──────────────────────────────────────────────────────
download_cloud_image() {
    if [ -f "${CLOUD_IMAGE}" ]; then
        echo "Cloud image already present: ${CLOUD_IMAGE}"
        return
    fi
    echo "Downloading Ubuntu 22.04 minimal cloud image..."
    mkdir -p "${VM_DIR}"
    curl -fL --progress-bar -o "${CLOUD_IMAGE}" "${CLOUD_IMAGE_URL}"
    echo "Downloaded: ${CLOUD_IMAGE}"
}

# ── create cloud-init seed ISO ────────────────────────────────────────────────
create_seed_iso() {
    local tmpdir
    tmpdir=$(mktemp -d)
    trap "rm -rf ${tmpdir}" RETURN

    # meta-data (required by cloud-init NoCloud datasource)
    cat >"${tmpdir}/meta-data" <<'EOF'
instance-id: wallhack-vm-setup
local-hostname: wallhack-vm
EOF

    # user-data: install packages, write wallhack-init, shut down
    # The init script content is embedded via heredoc from the repo.
    if [ ! -f "${INIT_SCRIPT}" ]; then
        echo "Error: ${INIT_SCRIPT} not found — cannot embed into cloud-init" >&2
        exit 1
    fi

    # Escape the init script for YAML block literal
    INIT_CONTENT=$(cat "${INIT_SCRIPT}")

    python3 - <<PYEOF > "${tmpdir}/user-data"
import sys, textwrap

init_content = open('${INIT_SCRIPT}').read()

# Indent each line with 4 spaces for YAML block literal
indented = '\n'.join('    ' + line for line in init_content.splitlines())

yaml = f"""#cloud-config
package_update: true
package_upgrade: false
packages:
  - socat
  - iproute2
  - tcpdump
  - iperf3
  - python3

write_files:
  - path: /usr/local/bin/wallhack-init
    permissions: '0755'
    content: |
{indented}

# Ensure wallhack-init is executable (belt + suspenders)
runcmd:
  - chmod +x /usr/local/bin/wallhack-init

power_state:
  mode: poweroff
  condition: True
  timeout: 5
"""
print(yaml)
PYEOF

    # Create the seed ISO
    if command -v cloud-localds >/dev/null; then
        cloud-localds "${SEED_ISO}" "${tmpdir}/user-data" "${tmpdir}/meta-data"
    elif command -v genisoimage >/dev/null; then
        genisoimage -output "${SEED_ISO}" -volid cidata -joliet -rock \
            "${tmpdir}/user-data" "${tmpdir}/meta-data"
    else
        mkisofs -output "${SEED_ISO}" -volid cidata -joliet -rock \
            "${tmpdir}/user-data" "${tmpdir}/meta-data"
    fi
    echo "Seed ISO created: ${SEED_ISO}"
}

# ── apply cloud-init by booting VM once ───────────────────────────────────────
apply_cloud_init() {
    echo "Creating working image from cloud base..."
    qemu-img convert -f qcow2 -O qcow2 "${CLOUD_IMAGE}" "${WORK_IMAGE}"
    qemu-img resize "${WORK_IMAGE}" 4G

    echo "Booting VM to apply cloud-init (this may take 2-5 minutes)..."
    echo "Watch for 'Cloud-init v.' messages followed by shutdown."

    # Boot with serial output visible; exit when VM powers off
    # shellcheck disable=SC2086
    qemu-system-x86_64 \
        ${KVM_FLAGS} \
        -m 1G \
        -smp 2 \
        -drive "file=${WORK_IMAGE},if=virtio,format=qcow2" \
        -drive "file=${SEED_ISO},if=virtio,format=raw,media=cdrom" \
        -nographic \
        -no-reboot \
        -serial stdio \
        -append "console=ttyS0 net.ifnames=0 biosdevname=0" \
        -netdev user,id=net0 \
        -device virtio-net-pci,netdev=net0

    echo "Cloud-init setup complete."
}

# ── save final image ──────────────────────────────────────────────────────────
save_base_image() {
    echo "Compacting and saving base image..."
    qemu-img convert -c -f qcow2 -O qcow2 "${WORK_IMAGE}" "${BASE_IMAGE}"
    rm -f "${WORK_IMAGE}" "${SEED_ISO}"
    echo ""
    echo "Base image ready: ${BASE_IMAGE}"
    echo "Size: $(du -sh "${BASE_IMAGE}" | cut -f1)"
}

# ── main ──────────────────────────────────────────────────────────────────────
main() {
    if [ -f "${BASE_IMAGE}" ]; then
        echo "Base image already exists: ${BASE_IMAGE}"
        echo "Delete it first to rebuild: rm ${BASE_IMAGE}"
        exit 0
    fi

    mkdir -p "${VM_DIR}"

    check_deps
    download_cloud_image
    create_seed_iso
    apply_cloud_init
    save_base_image

    echo ""
    echo "Setup complete. Run 'just smoke' to test."
}

main "$@"
