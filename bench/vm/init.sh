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
#   metric=tcp_upstream|tcp_downstream|udp|latency|parallel4|parallel8|...  (iperf3 metric)
#   duration=N (iperf3 test duration in seconds, default 5)
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
RATE=$(_param rate)
METRIC=$(_param metric)
DURATION=$(_param duration)
DEBUG=$(_param debug)

: "${ROLE:=entry}"
: "${SCENARIO:=smoke}"
: "${TRANSPORT:=quic}"
: "${METRIC:=tcp_upstream}"
: "${DURATION:=5}"

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
# TODO: replace with `wallhack peers --json | jq -r '.[0].tun_name'` once
# wallhack gains a --json flag (non-slim feature) and PeerInfo exposes tun_name.
# TUN name is wh + FNV-1a32(PEER_NAME), per peer_name_to_iface() in daemon/src/mode/entry.rs
TUN_NAME=wh5b770c26

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
	wallhack daemon ${DEBUG:+"--debug"} --role exit \
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

# ---------- benchmark / measurement ----------------------------------------

# Parse bits_per_second (→ Mbps, 3 decimal places) from a named section in
# iperf3 --json output. iperf3 3.20 uses tab after colon so we use match().
# $1 = section name: sum_sent | sum_received | sum
_iperf3_bps_mbps() {
	awk -v sec="$1" '
		$0 ~ ("\"" sec "\"") { in_sec = 1 }
		in_sec && /"bits_per_second"/ {
			match($0, /[0-9]+\.?[0-9]*/)
			printf "%.3f\n", substr($0, RSTART, RLENGTH) / 1e6
			exit
		}
	'
}

_run_latency_benchmark() {
	_RAW=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json 2>/dev/null)
	_VALUE=$(printf '%s' "${_RAW}" \
		| awk '/"mean_rtt"/{match($0,/[0-9]+/); v=substr($0,RSTART,RLENGTH)+0; printf "%.3f\n", v/1000; exit}')
	: "${_VALUE:=0}"
	_pass "\"metric\":\"latency\",\"value_ms\":${_VALUE}"
}

_run_benchmark() {
	if ! command -v iperf3 >/dev/null 2>&1; then
		_fail "iperf3 not found in initramfs"
		return
	fi

	if [ "${METRIC}" = "latency" ]; then
		_run_latency_benchmark
		return
	fi

	case "${METRIC}" in
		tcp_upstream)    _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json    | _iperf3_bps_mbps "sum_sent") ;;
		tcp_downstream)  _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json -R | _iperf3_bps_mbps "sum_received") ;;
		udp)             _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json -u -b 0 | _iperf3_bps_mbps "sum_sent") ;;
		parallel4)       _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json -P 4 | _iperf3_bps_mbps "sum_sent") ;;
		parallel8)       _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json -P 8 | _iperf3_bps_mbps "sum_sent") ;;
		parallel32)      _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json -P 32 | _iperf3_bps_mbps "sum_sent") ;;
		parallel64)      _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json -P 64 | _iperf3_bps_mbps "sum_sent") ;;
		parallel128)     _VALUE=$(iperf3 -c "${ECHO_PRIV}" -p "${IPERF3_PORT}" -t "${DURATION}" --json -P 128 | _iperf3_bps_mbps "sum_sent") ;;
		*)               _fail "unknown benchmark metric: ${METRIC}"; return ;;
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
	if [ -n "${LOSS}" ] || [ -n "${DELAY}" ] || [ -n "${RATE}" ]; then
		# Diagnostic: check tc + netem support
		echo "NETEM_DIAG: tc binary: $(which tc 2>&1)"
		echo "NETEM_DIAG: tc version: $(tc -V 2>&1)"
		echo "NETEM_DIAG: modinfo sch_netem: $(modinfo sch_netem 2>&1)"
		echo "NETEM_DIAG: loopback test: $(tc qdisc add dev lo root netem delay 1ms 2>&1; echo "exit=$?"; tc qdisc del dev lo root 2>/dev/null)"
		echo "NETEM_DIAG: applying: tc qdisc add dev ${IFACE} root netem ${DELAY:+delay $DELAY} ${LOSS:+loss $LOSS} ${RATE:+rate $RATE}"
		tc qdisc add dev "${IFACE}" root netem \
			${DELAY:+delay "$DELAY"} \
			${LOSS:+loss "$LOSS"} \
			${RATE:+rate "$RATE"} 2>&1
		echo "NETEM_DIAG: tc exit=$?"
		echo "NETEM_DIAG: qdisc show: $(tc qdisc show dev ${IFACE} 2>&1)"
	fi

	echo "WALLHACK_TS: entry_wallhack_start=$(date +%s%3N)"
	# Start wallhack entry node (listen mode)
	wallhack daemon ${DEBUG:+"--debug"} --role entry \
		-l ":${WH_PORT}${_TSUFFIX}" \
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
	else
		_run_benchmark
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
