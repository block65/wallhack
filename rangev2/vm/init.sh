#!/bin/busybox sh
set -e

# Mount virtual filesystems
mount -t proc none /proc
mount -t sysfs none /sys
mount -t devtmpfs none /dev 2>/dev/null || true

# Loopback
ip link set lo up
ip addr add 127.0.0.1/8 dev lo 2>/dev/null || true

# Parse /proc/cmdline — all net.* and svc.* args
NET_IPS=""
NET_GW=""
NET_MASQ=""
NET_FWD="0"
SVC_START=""
WH_ROLE=""
WH_NAME=""
WH_LISTEN=""
WH_CONNECT=""

for arg in $(cat /proc/cmdline); do
    case "$arg" in
        net.ip=*)               NET_IPS="${arg#*=}" ;;        # eth0:10.0.0.1/24,eth1:10.0.0.2/24
        net.gw=*)               NET_GW="${arg#*=}" ;;
        net.masquerade=*)       NET_MASQ="${arg#*=}" ;;       # eth1
        net.forward=*)          NET_FWD="${arg#*=}" ;;
        svc.start=*)            SVC_START="${arg#*=}" ;;      # ssh,wallhack
        svc.wallhack.role=*)    WH_ROLE="${arg#*=}" ;;
        svc.wallhack.name=*)    WH_NAME="${arg#*=}" ;;
        svc.wallhack.listen=*)  WH_LISTEN="${arg#*=}" ;;
        svc.wallhack.connect=*) WH_CONNECT="${arg#*=}" ;;
    esac
done

# Export wallhack args so layer start.sh scripts can reference them
export WH_ROLE WH_NAME WH_LISTEN WH_CONNECT

# Configure network interfaces
if [ -n "$NET_IPS" ]; then
    for entry in $(echo "$NET_IPS" | tr ',' ' '); do
        iface="${entry%%:*}"
        addr="${entry#*:}"
        ip link set "$iface" up
        ip addr add "$addr" dev "$iface"
    done
fi
[ -n "$NET_GW" ] && ip route add default via "$NET_GW"
[ "$NET_FWD" = "1" ] && echo 1 > /proc/sys/net/ipv4/ip_forward

# Match standard distro default (kernel ships 1 0 = disabled)
echo "0 2147483647" > /proc/sys/net/ipv4/ping_group_range

if [ -n "$NET_MASQ" ]; then
    iptables -t nat -A POSTROUTING -o "$NET_MASQ" -j MASQUERADE 2>/dev/null || true
fi

# Mount 9p host share for vm_inject support
mkdir -p /mnt/share
mount -t 9p -o trans=virtio,version=9p2000.L hostshare /mnt/share 2>/dev/null || true

# Start requested services — each in its own background subshell so slow inits
# (postgres initdb, mariadb install_db) don't delay BOOT_COMPLETE_V2
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
for svc in $(echo "$SVC_START" | tr ',' ' '); do
    script="/svc/${svc}/start.sh"
    if [ -x "$script" ]; then
        ( . "$script" ) &
    else
        echo "warning: no start.sh for service '$svc' (expected $script)" > /dev/ttyS0
    fi
done

# Signal ready to host (pontoon polls serial for this token)
echo "BOOT_COMPLETE_V2"

# Run interactive shell as child of PID 1 (so it respects signals like SIGINT)
# We loop to respawn if it exits.
export TERM=dumb

# Prevent init from consuming console input (which causes panic on Ctrl-D/EOF)
# Also trap signals to prevent init from exiting on Ctrl-C
trap '' INT QUIT TSTP
exec < /dev/null

while true; do
    # Explicitly attach to ttyS0 for the user shell (console)
    setsid -c sh -c 'stty sane; stty echo; exec sh -l' < /dev/ttyS0 > /dev/ttyS0 2>&1
    
    # If shell exits, restart
    echo "Shell exited, respawning..." > /dev/ttyS0
    sleep 1
done &

# Secondary shell on ttyS1 or hvc0 for MCP agent
# Runs in background, loops forever
while true; do
    MCP_TTY=""
    if [ -c /dev/hvc0 ]; then
        MCP_TTY="/dev/hvc0"
    elif [ -c /dev/ttyS1 ]; then
        MCP_TTY="/dev/ttyS1"
    else
        echo "Waiting for MCP TTY..." > /dev/ttyS0
        sleep 1
        continue
    fi
    
    echo "Starting MCP shell on $MCP_TTY..." > /dev/ttyS0
    
    # Configure serial port (ignore errors)
    if command -v stty >/dev/null; then
        stty -F $MCP_TTY 115200 cs8 -parenb -cstopb clocal cread >/dev/ttyS0 2>&1 || true
        stty -F $MCP_TTY -a > /dev/ttyS0 2>&1 || true
    fi
    
    # Try setsid -c (set controlling terminal)
    if setsid -c /bin/sh -l < $MCP_TTY > $MCP_TTY 2>&1; then
         # Success (blocked until exit)
         :
    else
         echo "setsid -c failed ($?), falling back to getty" > /dev/ttyS0
         # fallback to getty
         getty -L -n -l /bin/sh 115200 ${MCP_TTY#/dev/} vt100 || echo "getty failed ($?)" > /dev/ttyS0
    fi
    RET=$?
    
    echo "MCP Shell on $MCP_TTY exited (code $RET), respawning..." > /dev/ttyS0
    sleep 1
done
