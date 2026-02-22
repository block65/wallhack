#!/bin/sh
# wallhack-init — VM PID 1 init script (busybox-based, no python3)
#
# Invoked by the kernel as /init from the initramfs.
# Parses wallhack.* kernel cmdline params, configures networking, then
# dispatches to exit or entry role.
#
# Exit role  — start nc echo servers + wallhack exit, print
#              WALLHACK_EXIT_READY_MAGIC_TOKEN, then sleep forever.
# Entry role — start wallhack entry, wait for TUN, configure routing,
#              run transfer/benchmark, print WALLHACK_RESULT_MAGIC_TOKEN.
#
# Kernel cmdline params (all prefixed wallhack.):
#   role=exit|entry
#   scenario=smoke|resilience|benchmark|noop|debug-topology
#   transport=quic|websocket
#   loss=N%    (resilience: netem loss,  e.g. "2%")
#   delay=Nms  (resilience: netem delay, e.g. "25ms")
#   metric=tcp_fwd|tcp_rev|udp|latency|parallel2|parallel4  (benchmark only)
#   debug=1    (pass --debug to wallhack, keep running after test)

export PATH=/bin:/sbin:/usr/bin:/usr/sbin:/usr/local/bin

# ---------- poweroff helper -----------------------------------------------
do_poweroff() {
	sync
	poweroff -f 2>/dev/null || true
	sleep 2
	echo 1 > /proc/sys/kernel/sysrq 2>/dev/null || true
	echo o > /proc/sysrq-trigger   2>/dev/null || true
	sleep 30  # should never reach here
}

# ---------- global result-tracking for clean trap handling ----------------
_RESULT_PRINTED=0

_on_exit() {
	if [ "${_RESULT_PRINTED}" = "0" ] && [ "${_ROLE}" = "entry" ]; then
		echo "WALLHACK_RESULT_MAGIC_TOKEN: {\"status\":\"fail\",\"scenario\":\"${SCENARIO:-unknown}\",\"transport\":\"${TRANSPORT:-unknown}\",\"reason\":\"init script exited unexpectedly (check VM log)\"}"
	fi
	do_poweroff
}
trap '_on_exit' EXIT

# ---------- virtual filesystems -------------------------------------------
mount -t proc    proc    /proc
mount -t sysfs   sys     /sys
mount -t devtmpfs dev    /dev
mount -t tmpfs   tmpfs   /tmp
mdev -s

# ---------- loopback -------------------------------------------------------
ip link set lo up

# ---------- find the ethernet interface -----------------------------------
_find_eth() {
	for _i in $(seq 10); do
		IFACE=$(ip -o link show | awk -F': ' '$2 != "lo" && $2 !~ /^sit/ {print $2; exit}')
		if [ -n "$IFACE" ]; then
			echo "$IFACE"
			return
		fi
		sleep 0.1
	done
}

IFACE=$(_find_eth)
if [ -z "${IFACE}" ]; then
	echo "WALLHACK_RESULT_MAGIC_TOKEN: {\"status\":\"fail\",\"reason\":\"no ethernet interface found\"}"
	_RESULT_PRINTED=1
	do_poweroff
fi

# ---------- parse wallhack.* params from /proc/cmdline --------------------
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

# ---------- network layout ------------------------------------------------
# Both VMs share an L2 segment via QEMU socket networking.
#   exit  VM IFACE:  10.0.0.1/24   (net.ifnames=0 ensures eth0)
#   entry VM IFACE:  10.0.0.2/24
#
# Private address on exit loopback: 10.100.0.1/32
#   — used by socat/iperf3 echo servers
#   — NOT reachable from entry IFACE directly; only via the tunnel
EXIT_ETH=10.0.0.1
ENTRY_ETH=10.0.0.2
ECHO_PRIV=10.100.0.1
ENTRY_TUN_IP=10.100.0.2
ECHO_TCP_PORT=9999
ECHO_UDP_PORT=9998
IPERF3_PORT=5201
WH_PORT=6565
PEER_NAME=vm
TUN_NAME=tun-${PEER_NAME}

# Transport suffix: QUIC = "" (UDP), WebSocket = "/tcp"
_TSUFFIX=""
if [ "${TRANSPORT}" = "websocket" ]; then
	_TSUFFIX="/tcp"
fi

# ---------- exit role =====================================================
_run_exit() {
	echo "WALLHACK_TS: exit_net_start=$(date +%s%3N)"
	ip addr add "${EXIT_ETH}/24" dev "${IFACE}"
	ip link set "${IFACE}" up

	# Private tunnel-only address for echo servers
	ip addr add "${ECHO_PRIV}/32" dev lo

	# TCP echo server (robust, forks for each connection)
	socat TCP4-LISTEN:"${ECHO_TCP_PORT}",bind="${ECHO_PRIV}",reuseaddr,fork EXEC:/bin/cat &

	# UDP echo server
	socat UDP4-RECVFROM:"${ECHO_UDP_PORT}",bind="${ECHO_PRIV}",reuseaddr,fork EXEC:/bin/cat &

	# iperf3 server (optional — only needed for benchmark scenario)
	if command -v iperf3 >/dev/null 2>&1; then
		iperf3 -s -B "${ECHO_PRIV}" -p "${IPERF3_PORT}" \
		       --logfile /tmp/iperf3-server.log &
	fi

	echo "WALLHACK_TS: exit_wallhack_start=$(date +%s%3N)"
	# wallhack exit node — connects to entry (retries with backoff)
	wallhack ${DEBUG:+"--debug"} exit \
		-c "${ENTRY_ETH}:${WH_PORT}${_TSUFFIX}" \
		--name "${PEER_NAME}" \
		2>&1 | tee /tmp/wallhack-exit.log &

	# Wait for services to bind instead of arbitrary sleep
	_waited=0
	until nc -z "${ECHO_PRIV}" "${ECHO_TCP_PORT}" >/dev/null 2>&1; do
		if [ ${_waited} -ge 10 ]; then
			echo "WALLHACK_RESULT_MAGIC_TOKEN: {\"status\":\"fail\",\"reason\":\"exit services failed to bind\"}"
			_RESULT_PRINTED=1
			do_poweroff
		fi
		sleep 0.1
		_waited=$((_waited + 1))
	done

	echo "WALLHACK_TS: exit_services_ready=$(date +%s%3N)"
	# Signal the host: network configured, services started
	echo "WALLHACK_EXIT_READY_MAGIC_TOKEN"

	# Stay alive as PID 1 until the host kills QEMU.
	# Clear the EXIT trap first so we don't print a spurious failure.
	trap - EXIT
	exec sleep infinity
}

# ---------- entry role helpers ============================================

# Wait up to $1 seconds for TUN interface $TUN_NAME to appear.
_wait_for_tun() {
	_timeout=${1:-45} _elapsed=0
	until ip link show "${TUN_NAME}" >/dev/null 2>&1; do
		if [ ${_elapsed} -ge $((_timeout * 10)) ]; then
			_fail "TUN ${TUN_NAME} did not appear within ${_timeout}s"
		fi
		sleep 0.1
		_elapsed=$((_elapsed + 1))
	done
}

# Print a failure result and power off.
_fail() {
	_reason="${1:-unknown error}"
	_T_END=$(date +%s)
	_DUR=$((_T_END - _T0))
	echo "=== ip addr ==="
	ip addr 2>/dev/null
	echo "=== ip route ==="
	ip route 2>/dev/null
	echo "=== wallhack-entry log ==="
	cat /tmp/wallhack-entry.log 2>/dev/null
	echo "=== socat-tcp log ==="
	cat /tmp/socat-tcp.log 2>/dev/null
	echo "WALLHACK_RESULT_MAGIC_TOKEN: {\"status\":\"fail\",\"scenario\":\"${SCENARIO}\",\"transport\":\"${TRANSPORT}\",\"reason\":\"${_reason}\",\"duration_s\":${_DUR}}"
	_RESULT_PRINTED=1
	do_poweroff
}

# Print a pass result and power off (or stay up for debug-topology).
_pass() {
	_T_END=$(date +%s)
	_DUR=$((_T_END - _T0))
	_EXTRA="${1}"
	if [ -n "${_EXTRA}" ]; then
		echo "WALLHACK_RESULT_MAGIC_TOKEN: {\"status\":\"pass\",\"scenario\":\"${SCENARIO}\",\"transport\":\"${TRANSPORT}\",${_EXTRA},\"duration_s\":${_DUR}}"
	else
		echo "WALLHACK_RESULT_MAGIC_TOKEN: {\"status\":\"pass\",\"scenario\":\"${SCENARIO}\",\"transport\":\"${TRANSPORT}\",\"duration_s\":${_DUR}}"
	fi
	_RESULT_PRINTED=1
	if [ "${DEBUG}" = "1" ]; then
		trap - EXIT
		exec sleep infinity
	fi
	do_poweroff
}

# ---------- smoke / resilience transfer test ------------------------------
_run_transfer_test() {
	# TCP: 64 KB deterministic payload (all-zero), sha256 round-trip verification
	dd if=/dev/zero bs=65536 count=1 of=/tmp/payload.bin 2>/dev/null
	EXPECTED=$(sha256sum /tmp/payload.bin | awk '{print $1}')

	if ! socat -T 30 - TCP4:"${ECHO_PRIV}":"${ECHO_TCP_PORT}",bind="${ENTRY_TUN_IP}",shut-down \
			< /tmp/payload.bin > /tmp/response.bin 2>/tmp/socat-tcp.log; then
		_TCP_ERR=$(head -1 /tmp/socat-tcp.log | tr '"' "'")
		_fail "TCP transfer failed: ${_TCP_ERR}"
		return
	fi

	ACTUAL=$(sha256sum /tmp/response.bin | awk '{print $1}')
	if [ "${EXPECTED}" != "${ACTUAL}" ]; then
		_TCP_SZ=$(wc -c < /tmp/response.bin)
		_fail "TCP sha256 mismatch: sent=${EXPECTED} got=${ACTUAL} response_size=${_TCP_SZ}"
		return
	fi

	# UDP: 64-byte deterministic payload (0x55 = 'U'), echo verification.
	# Use nc -u instead of socat to avoid socat's EOF-signalling behaviour
	# (socat sends a 0-byte UDP datagram when stdin closes, which races
	# against the real echo response arriving through the tunnel).
	head -c 64 /dev/zero | tr '\0' 'U' > /tmp/udp-payload.bin
	EXPECTED_UDP=$(sha256sum /tmp/udp-payload.bin | awk '{print $1}')

	if ! nc -u -w 1 "${ECHO_PRIV}" "${ECHO_UDP_PORT}" \
		< /tmp/udp-payload.bin > /tmp/udp-response.bin; then
		_fail "UDP echo failed (nc exit $?)"
		return
	fi

	ACTUAL_UDP=$(sha256sum /tmp/udp-response.bin | awk '{print $1}')
	if [ "${EXPECTED_UDP}" != "${ACTUAL_UDP}" ]; then
		_UDP_SZ=$(wc -c < /tmp/udp-response.bin)
		echo "=== udp-response hexdump ==="
		od -A x -t x1z /tmp/udp-response.bin 2>/dev/null || true
		if [ "${_UDP_SZ}" -eq 0 ]; then
			_fail "UDP echo: response was empty (0 bytes) — tunnel may not be forwarding UDP; check wallhack-entry.log above"
		else
			_fail "UDP echo mismatch: expected=${EXPECTED_UDP} got=${ACTUAL_UDP} response_size=${_UDP_SZ}"
		fi
		return
	fi

	_pass
}

# ---------- benchmark test ------------------------------------------------

_iperf3_mbps() {
	awk "/$1/{for(i=1;i<=NF;i++) if(\$i==\"Mbits/sec\") {printf \"%.2f\\n\", \$(i-1); exit}}"
}

_run_latency_benchmark() {
	if ! _PING_OUT=$(ping -c 20 -q "${ECHO_PRIV}" 2>&1); then
		_fail "ping failed: $(echo "${_PING_OUT}" | head -1 | tr '"' "'")"
		return
	fi
	_AVG=$(printf '%s' "${_PING_OUT}" | awk -F'[=/]' '/rtt/{print $5}')
	: "${_AVG:=0}"
	_pass "\"metric\":\"latency\",\"value_ms\":${_AVG}"
}

_run_benchmark() {
	if [ "${METRIC}" = "latency" ]; then
		_run_latency_benchmark
		return
	fi

	if ! command -v iperf3 >/dev/null 2>&1; then
		_fail "iperf3 not found in initramfs"
		return
	fi

	case "${METRIC}" in
		tcp_fwd)   _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t 10 -f m    | _iperf3_mbps "sender") ;;
		tcp_rev)   _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t 10 -f m -R | _iperf3_mbps "receiver") ;;
		udp)       _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t 10 -f m -u -b 0 | _iperf3_mbps "sender") ;;
		parallel2) _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t 10 -f m -P 2 | _iperf3_mbps "SUM.*sender") ;;
		parallel4) _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t 10 -f m -P 4 | _iperf3_mbps "SUM.*sender") ;;
		*)         _fail "unknown benchmark metric: ${METRIC}"; return ;;
	esac

	: "${_VALUE:=0}"
	_pass "\"metric\":\"${METRIC}\",\"value_mbps\":${_VALUE}"
}

# ---------- entry role ====================================================
_run_entry() {
	_T0=$(date +%s)
	echo "WALLHACK_TS: entry_net_start=$(date +%s%3N)"

	ip addr add "${ENTRY_ETH}/24" dev "${IFACE}"
	ip link set "${IFACE}" up

	# Apply netem on eth0 BEFORE starting wallhack (resilience scenarios)
	if [ -n "${LOSS}" ] || [ -n "${DELAY}" ]; then
		tc qdisc add dev "${IFACE}" root netem \
			${DELAY:+delay "$DELAY"} \
			${LOSS:+loss "$LOSS"}
	fi

	echo "WALLHACK_TS: entry_wallhack_start=$(date +%s%3N)"
	# Start wallhack entry node (listen mode)
	wallhack ${DEBUG:+"--debug"} entry \
		-l "0.0.0.0:${WH_PORT}${_TSUFFIX}" \
		2>&1 | tee /tmp/wallhack-entry.log &

	# Wait for wallhack to bind the listen port by watching /proc/net directly.
	# Avoids sleeps: polls at 10ms until the kernel shows the socket as bound.
	# Port in hex for /proc/net/{udp,tcp} lookup (big-endian, uppercase).
	_PORT_HEX=$(printf '%04X' "${WH_PORT}")
	_elapsed=0
	if [ "${TRANSPORT}" = "websocket" ]; then
		until grep -q ":${_PORT_HEX} " /proc/net/tcp /proc/net/tcp6 2>/dev/null; do
			if [ ${_elapsed} -ge 100 ]; then
				_fail "wallhack entry failed to bind TCP port ${WH_PORT} within 10s"
			fi
			sleep 0.1
			_elapsed=$((_elapsed + 1))
		done
	else
		until grep -q ":${_PORT_HEX} " /proc/net/udp /proc/net/udp6 2>/dev/null; do
			if [ ${_elapsed} -ge 100 ]; then
				_fail "wallhack entry failed to bind UDP port ${WH_PORT} within 10s"
			fi
			sleep 0.1
			_elapsed=$((_elapsed + 1))
		done
	fi
	echo "WALLHACK_TS: entry_port_bound=$(date +%s%3N)"
	# Signal the host: entry is listening, safe to start exit VM
	echo "WALLHACK_ENTRY_READY_MAGIC_TOKEN"

	# Wait for TUN to appear (created when exit node connects)
	_wait_for_tun 5
	echo "WALLHACK_TS: entry_tun_up=$(date +%s%3N)"

	# Configure TUN
	ip link set "${TUN_NAME}" up
	ip addr add "${ENTRY_TUN_IP}/32" dev "${TUN_NAME}"
	ip route add "${ECHO_PRIV}/32" dev "${TUN_NAME}"

	# Gate: wait for the data path through the tunnel to be ready.
	# The TUN interface existing is necessary but not sufficient — both
	# sides need their routes configured before traffic can flow.
	_elapsed=0
	until ping -c 1 -W 1 "${ECHO_PRIV}" >/dev/null 2>&1; do
		if [ ${_elapsed} -ge 300 ]; then
			_fail "Tunnel data path not ready after 30s (no ICMP reply from ${ECHO_PRIV})"
		fi
		sleep 0.1
		_elapsed=$((_elapsed + 1))
	done
	echo "WALLHACK_TS: entry_tunnel_ready=$(date +%s%3N)"

	if [ "${SCENARIO}" = "noop" ]; then
		_pass
	elif [ "${SCENARIO}" = "benchmark" ]; then
		_run_benchmark
	else
		_run_transfer_test
	fi
}

# ---------- role dispatch -------------------------------------------------
case "${ROLE}" in
	exit)  _run_exit  ;;
	entry) _run_entry ;;
	*)
		echo "WALLHACK_RESULT_MAGIC_TOKEN: {\"status\":\"fail\",\"reason\":\"unknown role: ${ROLE}\"}"
		_RESULT_PRINTED=1
		do_poweroff
		;;
esac
