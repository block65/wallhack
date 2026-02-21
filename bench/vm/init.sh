#!/bin/sh
# wallhack-init — VM PID 1 init script
#
# Invoked by the kernel as init=/usr/local/bin/wallhack-init.
# Parses wallhack.* kernel cmdline params, configures networking, then
# dispatches to exit or entry role.
#
# Exit role  — start socat echo servers + wallhack exit, print
#              WALLHACK_EXIT_READY, then sleep forever (killed by host).
# Entry role — start wallhack entry, wait for TUN, configure routing,
#              run transfer/benchmark, print WALLHACK_RESULT, then power off.
#
# Kernel cmdline params (all prefixed wallhack.):
#   role=exit|entry
#   scenario=smoke|resilience|benchmark|debug-topology
#   transport=quic|websocket
#   loss=N%    (resilience/benchmark-lossy: netem loss,  e.g. "2%")
#   delay=Nms  (resilience/benchmark-lossy: netem delay, e.g. "25ms")
#   metric=tcp_fwd|tcp_rev|udp|latency|parallel2|parallel4  (benchmark only)
#   debug=1    (wallhack --debug verbosity, keep running after test)

# ---------- poweroff helper (works as PID 1, no systemd required) --------
do_poweroff() {
    sync
    # sysrq 'o' = immediate power-off
    echo 1 > /proc/sys/kernel/sysrq 2>/dev/null || true
    echo o > /proc/sysrq-trigger   2>/dev/null || true
    sleep 3
    # fallback: reboot(2) via python3 (RB_POWER_OFF = 0x4321fedc)
    python3 -c \
        "import ctypes; ctypes.CDLL('libc.so.6').reboot(0x4321fedc)" \
        2>/dev/null || true
    sleep 60  # should never reach here
}

# ---------- global result-tracking for clean trap handling ---------------
_RESULT_PRINTED=0

_on_exit() {
    if [ "${_RESULT_PRINTED}" = "0" ] && [ "${_ROLE}" = "entry" ]; then
        echo "WALLHACK_RESULT: {\"status\":\"fail\",\"scenario\":\"${SCENARIO:-unknown}\",\"transport\":\"${TRANSPORT:-unknown}\",\"reason\":\"init script exited unexpectedly (check VM log)\"}"
    fi
    do_poweroff
}
trap '_on_exit' EXIT

# ---------- virtual filesystems -----------------------------------------
mount -t proc    proc    /proc
mount -t sysfs   sys     /sys
mount -t devtmpfs dev    /dev
mkdir -p /dev/pts
mount -t devpts  devpts  /dev/pts
mount -t tmpfs   tmpfs   /tmp

# ---------- loopback -----------------------------------------------------
ip link set lo up

# ---------- 9p share: wallhack repo (read-only) --------------------------
mkdir -p /wallhack
mount -t 9p -o trans=virtio,version=9p2000.L,ro wallhack /wallhack

# ---------- install wallhack binary from share ---------------------------
install -m 755 /wallhack/target/release/wallhack /usr/local/bin/wallhack

# ---------- parse wallhack.* params from /proc/cmdline -------------------
cmdline=$(cat /proc/cmdline)

_param() {
    printf '%s' "${cmdline}" | tr ' ' '\n' \
        | grep "^wallhack\.$1=" | cut -d= -f2- | head -1
}

ROLE=$(_param role)
SCENARIO=$(_param scenario)
TRANSPORT=$(_param transport)
LOSS=$(_param loss)
DELAY=$(_param delay)
METRIC=$(_param metric)
DEBUG=$(_param debug)

: "${ROLE:=entry}"
: "${SCENARIO:=smoke}"
: "${TRANSPORT:=quic}"
: "${METRIC:=tcp_fwd}"

# Expose _ROLE to the exit trap
_ROLE="${ROLE}"

# ---------- wallhack verbosity -------------------------------------------
WH_FLAGS=""
if [ "${DEBUG}" = "1" ]; then
    WH_FLAGS="--debug"
fi

# ---------- network layout -----------------------------------------------
# Both VMs share an L2 segment via QEMU socket networking.
#   exit  VM eth0:  10.0.0.1/24
#   entry VM eth0:  10.0.0.2/24
#
# Private address on exit loopback: 10.100.0.1/32
#   — used by socat/iperf3 echo servers
#   — NOT reachable from entry eth0 directly; only via the tunnel
#   — entry routes 10.100.0.1/32 via tun-vm after it comes up
EXIT_ETH=10.0.0.1
ENTRY_ETH=10.0.0.2
ECHO_PRIV=10.100.0.1
ENTRY_TUN_IP=10.100.0.2   # source IP assigned to entry's TUN
ECHO_TCP_PORT=9999
ECHO_UDP_PORT=9998
IPERF3_PORT=5201
WH_PORT=6565
PEER_NAME=vm              # exit node --name; entry creates tun-vm
TUN_NAME=tun-${PEER_NAME}

# Transport suffix: QUIC = "" (UDP), WebSocket = "/tcp"
_TSUFFIX=""
if [ "${TRANSPORT}" = "websocket" ]; then
    _TSUFFIX="/tcp"
fi

# ---------- exit role ====================================================
_run_exit() {
    ip addr add "${EXIT_ETH}/24" dev eth0
    ip link set eth0 up

    # Private tunnel-only address for echo server
    ip addr add "${ECHO_PRIV}/32" dev lo

    # TCP echo server
    socat TCP4-LISTEN:${ECHO_TCP_PORT},bind=${ECHO_PRIV},reuseaddr,fork \
          EXEC:/bin/cat &

    # UDP echo server
    socat UDP4-RECVFROM:${ECHO_UDP_PORT},bind=${ECHO_PRIV},reuseaddr,fork \
          EXEC:/bin/cat &

    # iperf3 server (if available — required for benchmark scenario)
    if [ -x /wallhack/bench/bin/iperf3 ]; then
        install -m 755 /wallhack/bench/bin/iperf3 /usr/local/bin/iperf3
    fi
    if command -v iperf3 >/dev/null 2>&1; then
        iperf3 -s -B "${ECHO_PRIV}" -p ${IPERF3_PORT} \
               --logfile /tmp/iperf3-server.log &
    fi

    # wallhack exit node — connects to entry (retries automatically)
    # shellcheck disable=SC2086
    wallhack ${WH_FLAGS} exit \
        -c "${ENTRY_ETH}:${WH_PORT}${_TSUFFIX}" \
        --name "${PEER_NAME}" \
        >/tmp/wallhack-exit.log 2>&1 &

    # Signal the host: QEMU L2 socket bound, services started
    echo "WALLHACK_EXIT_READY"

    # Stay alive as PID 1 until the host kills QEMU.
    # Clear the EXIT trap first so we don't print a spurious failure.
    trap - EXIT
    exec sleep infinity
}

# ---------- entry role helpers ===========================================

# Wait up to $1 seconds for TUN interface $TUN_NAME to appear.
_wait_for_tun() {
    _timeout=${1:-45}
    _waited=0
    until ip link show "${TUN_NAME}" >/dev/null 2>&1; do
        if [ ${_waited} -ge ${_timeout} ]; then
            _fail "TUN ${TUN_NAME} did not appear within ${_timeout}s"
        fi
        sleep 1
        _waited=$((_waited + 1))
    done
}

# Print a failure result and power off.
_fail() {
    _reason="${1:-unknown error}"
    _T_END=$(date +%s)
    _DUR=$((_T_END - _T0))
    echo "WALLHACK_RESULT: {\"status\":\"fail\",\"scenario\":\"${SCENARIO}\",\"transport\":\"${TRANSPORT}\",\"reason\":\"${_reason}\",\"duration_s\":${_DUR}}"
    _RESULT_PRINTED=1
    do_poweroff
}

# Print a pass result and power off (or stay up for debug-topology).
_pass() {
    _T_END=$(date +%s)
    _DUR=$((_T_END - _T0))
    _EXTRA="${1}"  # optional extra JSON fields (no leading comma needed here)
    if [ -n "${_EXTRA}" ]; then
        echo "WALLHACK_RESULT: {\"status\":\"pass\",\"scenario\":\"${SCENARIO}\",\"transport\":\"${TRANSPORT}\",${_EXTRA},\"duration_s\":${_DUR}}"
    else
        echo "WALLHACK_RESULT: {\"status\":\"pass\",\"scenario\":\"${SCENARIO}\",\"transport\":\"${TRANSPORT}\",\"duration_s\":${_DUR}}"
    fi
    _RESULT_PRINTED=1
    if [ "${DEBUG}" = "1" ]; then
        # debug-topology: keep running so developer can inspect
        trap - EXIT
        exec sleep infinity
    fi
    do_poweroff
}

# ---------- smoke / resilience transfer test -----------------------------
_run_transfer_test() {
    # TCP: 1 MB payload, sha256 round-trip verification
    dd if=/dev/urandom bs=1048576 count=1 of=/tmp/payload.bin 2>/dev/null
    EXPECTED=$(sha256sum /tmp/payload.bin | awk '{print $1}')

    if ! socat -T 30 - TCP4:"${ECHO_PRIV}":"${ECHO_TCP_PORT}" \
            </tmp/payload.bin >/tmp/response.bin 2>/tmp/socat-tcp.log; then
        _TCP_ERR=$(head -1 /tmp/socat-tcp.log 2>/dev/null | tr '"' "'")
        _fail "TCP transfer failed: ${_TCP_ERR}"
        return
    fi

    ACTUAL=$(sha256sum /tmp/response.bin | awk '{print $1}')
    if [ "${EXPECTED}" != "${ACTUAL}" ]; then
        _fail "TCP sha256 mismatch (sent ${EXPECTED}, got ${ACTUAL})"
        return
    fi

    # UDP: 64-byte payload, echo verification
    if ! python3 - <<PYEOF
import socket, sys
payload = b'wallhack-udp-test-' + b'x' * 46  # exactly 64 bytes
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(10)
try:
    s.sendto(payload, ('${ECHO_PRIV}', ${ECHO_UDP_PORT}))
    data, _ = s.recvfrom(256)
    if data != payload:
        print('mismatch: sent=%r got=%r' % (payload, data), file=sys.stderr)
        sys.exit(1)
finally:
    s.close()
PYEOF
    then
        _fail "UDP echo mismatch"
        return
    fi

    _pass
}

# ---------- benchmark test -----------------------------------------------
_run_benchmark() {
    if [ -x /wallhack/bench/bin/iperf3 ]; then
        install -m 755 /wallhack/bench/bin/iperf3 /usr/local/bin/iperf3
    fi

    if [ "${METRIC}" = "latency" ]; then
        _run_latency_benchmark
        return
    fi

    if ! command -v iperf3 >/dev/null 2>&1; then
        _fail "iperf3 not found in VM image"
        return
    fi

    case "${METRIC}" in
        tcp_fwd)   _IPERF_FLAGS="-c ${ECHO_PRIV} -p ${IPERF3_PORT} -t 10 -J" ;;
        tcp_rev)   _IPERF_FLAGS="-c ${ECHO_PRIV} -p ${IPERF3_PORT} -t 10 -J -R" ;;
        udp)       _IPERF_FLAGS="-c ${ECHO_PRIV} -p ${IPERF3_PORT} -t 10 -J -u -b 0" ;;
        parallel2) _IPERF_FLAGS="-c ${ECHO_PRIV} -p ${IPERF3_PORT} -t 10 -J -P 2" ;;
        parallel4) _IPERF_FLAGS="-c ${ECHO_PRIV} -p ${IPERF3_PORT} -t 10 -J -P 4" ;;
        *)         _fail "unknown benchmark metric: ${METRIC}"; return ;;
    esac

    # shellcheck disable=SC2086
    if ! iperf3 ${_IPERF_FLAGS} >/tmp/iperf3-result.json 2>/tmp/iperf3-error.log; then
        _ERR=$(head -1 /tmp/iperf3-error.log 2>/dev/null | tr '"' "'")
        _fail "iperf3 failed: ${_ERR}"
        return
    fi

    _VALUE=$(python3 - <<PYEOF 2>/dev/null || echo "0"
import json, sys
d = json.load(open('/tmp/iperf3-result.json'))
metric = '${METRIC}'
if metric == 'tcp_rev':
    bps = d['end']['sum_received']['bits_per_second']
elif metric == 'udp':
    bps = d['end']['sum']['bits_per_second']
elif metric in ('parallel2', 'parallel4'):
    bps = d['end']['sum_sent']['bits_per_second']
else:
    bps = d['end']['sum_sent']['bits_per_second']
print(f'{bps / 1e6:.2f}')
PYEOF
    )

    _pass "\"metric\":\"${METRIC}\",\"value_mbps\":${_VALUE}"
}

_run_latency_benchmark() {
    # ICMP RTT through the tunnel (wallhack supports ICMP on Unix)
    if ! _PING_OUT=$(ping -c 20 -q "${ECHO_PRIV}" 2>&1); then
        _fail "ping failed: $(echo "${_PING_OUT}" | head -1 | tr '"' "'")"
        return
    fi
    # "rtt min/avg/max/mdev = 0.123/0.456/0.789/0.100 ms"
    _AVG=$(printf '%s' "${_PING_OUT}" | awk -F'[=/]' '/rtt/{print $5}')
    : "${_AVG:=0}"
    _pass "\"metric\":\"latency\",\"value_ms\":${_AVG}"
}

# ---------- entry role ===================================================
_run_entry() {
    ip addr add "${ENTRY_ETH}/24" dev eth0
    ip link set eth0 up

    # Apply netem on eth0 BEFORE starting wallhack
    if [ -n "${LOSS}" ] || [ -n "${DELAY}" ]; then
        _NETEM=""
        [ -n "${DELAY}" ] && _NETEM="${_NETEM} delay ${DELAY}"
        [ -n "${LOSS}" ]  && _NETEM="${_NETEM} loss ${LOSS}"
        tc qdisc add dev eth0 root netem ${_NETEM}
    fi

    # Start wallhack entry node (listen mode)
    # shellcheck disable=SC2086
    wallhack ${WH_FLAGS} entry \
        -l ":${WH_PORT}${_TSUFFIX}" \
        >/tmp/wallhack-entry.log 2>&1 &

    # Wait for TUN to appear (created when exit node connects)
    _wait_for_tun 45

    # Configure TUN
    ip link set "${TUN_NAME}" up
    ip addr add "${ENTRY_TUN_IP}/32" dev "${TUN_NAME}"
    ip route add "${ECHO_PRIV}/32" dev "${TUN_NAME}"

    _T0=$(date +%s)

    if [ "${SCENARIO}" = "benchmark" ]; then
        _run_benchmark
    else
        _run_transfer_test
    fi
}

# ---------- role dispatch ------------------------------------------------
case "${ROLE}" in
    exit)  _run_exit  ;;
    entry) _run_entry ;;
    *)
        echo "WALLHACK_RESULT: {\"status\":\"fail\",\"reason\":\"unknown role: ${ROLE}\"}"
        _RESULT_PRINTED=1
        do_poweroff
        ;;
esac
