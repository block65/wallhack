#!/usr/bin/env bash
# Proper 4-namespace TCP tunnel diagnostic
# Topology:
#   wh-client (10.200.0.10) <--veth--> wh-entry (10.200.0.1 + TUN)
#   wh-entry (10.200.1.10) <--veth--> wh-exit (10.200.1.20)  
#   wh-exit (10.200.2.20) <--veth--> wh-target (10.200.2.10)
#
# Traffic flow: client -> entry TUN -> smoltcp -> QUIC -> exit -> target

set -euo pipefail

# Namespaces
NS_CLIENT="wh-client"
NS_ENTRY="wh-entry"
NS_EXIT="wh-exit"
NS_TARGET="wh-target"

# IPs
IP_CLIENT="10.200.0.10"
IP_ENTRY_CLIENT_SIDE="10.200.0.1"
IP_ENTRY_EXIT_SIDE="10.200.1.10"
IP_EXIT_ENTRY_SIDE="10.200.1.20"
IP_EXIT_TARGET_SIDE="10.200.2.20"
IP_TARGET="10.200.2.10"
PREFIX=24

# Ports and names
WALLHACK_PORT=6565
EXIT_ID="bench"
TUN_NAME="tun-${EXIT_ID}"
ECHO_PORT=9999

WALLHACK_BIN="${1:-./target/release/wallhack}"
LOG_DIR="/tmp"

cleanup() {
    echo "[*] Cleaning up..."
    [[ -n "${ENTRY_PID:-}" ]] && kill "$ENTRY_PID" 2>/dev/null || true
    [[ -n "${EXIT_PID:-}" ]] && kill "$EXIT_PID" 2>/dev/null || true
    [[ -n "${ECHO_PID:-}" ]] && kill "$ECHO_PID" 2>/dev/null || true
    # Give processes a moment to exit, then force kill any stragglers
    sleep 1
    jobs -p 2>/dev/null | xargs -r kill 2>/dev/null || true
    
    ip netns del "$NS_CLIENT" 2>/dev/null || true
    ip netns del "$NS_ENTRY" 2>/dev/null || true
    ip netns del "$NS_EXIT" 2>/dev/null || true
    ip netns del "$NS_TARGET" 2>/dev/null || true
    echo "[*] Cleanup complete"
}
trap cleanup EXIT

echo "=== Wallhack TCP Tunnel Diagnostic (4-namespace) ==="
echo "Binary: $WALLHACK_BIN"
echo

if [[ ! -x "$WALLHACK_BIN" ]]; then
    echo "ERROR: wallhack binary not found: $WALLHACK_BIN"
    exit 1
fi

# Step 1: Create namespaces
echo "[1/8] Creating network namespaces..."
for ns in "$NS_CLIENT" "$NS_ENTRY" "$NS_EXIT" "$NS_TARGET"; do
    ip netns del "$ns" 2>/dev/null || true
    ip netns add "$ns"
    ip netns exec "$ns" ip link set lo up
done

# Step 2: Create veth pairs
echo "[2/8] Creating veth pairs..."

# client <-> entry
ip link add veth-ce-client type veth peer name veth-ce-entry
ip link set veth-ce-client netns "$NS_CLIENT"
ip link set veth-ce-entry netns "$NS_ENTRY"
ip netns exec "$NS_CLIENT" ip addr add "${IP_CLIENT}/${PREFIX}" dev veth-ce-client
ip netns exec "$NS_ENTRY" ip addr add "${IP_ENTRY_CLIENT_SIDE}/${PREFIX}" dev veth-ce-entry
ip netns exec "$NS_CLIENT" ip link set veth-ce-client up
ip netns exec "$NS_ENTRY" ip link set veth-ce-entry up

# entry <-> exit (for QUIC tunnel)
ip link add veth-ee-entry type veth peer name veth-ee-exit
ip link set veth-ee-entry netns "$NS_ENTRY"
ip link set veth-ee-exit netns "$NS_EXIT"
ip netns exec "$NS_ENTRY" ip addr add "${IP_ENTRY_EXIT_SIDE}/${PREFIX}" dev veth-ee-entry
ip netns exec "$NS_EXIT" ip addr add "${IP_EXIT_ENTRY_SIDE}/${PREFIX}" dev veth-ee-exit
ip netns exec "$NS_ENTRY" ip link set veth-ee-entry up
ip netns exec "$NS_EXIT" ip link set veth-ee-exit up

# exit <-> target
ip link add veth-et-exit type veth peer name veth-et-target
ip link set veth-et-exit netns "$NS_EXIT"
ip link set veth-et-target netns "$NS_TARGET"
ip netns exec "$NS_EXIT" ip addr add "${IP_EXIT_TARGET_SIDE}/${PREFIX}" dev veth-et-exit
ip netns exec "$NS_TARGET" ip addr add "${IP_TARGET}/${PREFIX}" dev veth-et-target
ip netns exec "$NS_EXIT" ip link set veth-et-exit up
ip netns exec "$NS_TARGET" ip link set veth-et-target up

# Enable forwarding in entry and exit (required for routing)
ip netns exec "$NS_ENTRY" sysctl -qw net.ipv4.ip_forward=1
ip netns exec "$NS_EXIT" sysctl -qw net.ipv4.ip_forward=1

# Client routes traffic to target via entry
ip netns exec "$NS_CLIENT" ip route add 10.200.2.0/24 via "$IP_ENTRY_CLIENT_SIDE"

echo "[3/8] Starting echo server in target namespace..."
ip netns exec "$NS_TARGET" python3 -u -c "
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('0.0.0.0', $ECHO_PORT))
s.listen(1)
print(f'Echo server listening on port $ECHO_PORT', flush=True)
while True:
    conn, addr = s.accept()
    print(f'Connection from {addr}', flush=True)
    while True:
        data = conn.recv(4096)
        if not data:
            break
        conn.sendall(data)
    conn.close()
" &
ECHO_PID=$!
sleep 0.5

# Verify echo server reachable from exit
echo "[*] Verifying echo server reachable from exit namespace..."
if ip netns exec "$NS_EXIT" timeout 2 bash -c "echo test | nc -w1 $IP_TARGET $ECHO_PORT" >/dev/null 2>&1; then
    echo "    OK: Echo server reachable from exit"
else
    echo "    WARNING: Echo server not reachable from exit namespace"
fi

echo "[4/8] Starting wallhack ENTRY node..."
RUST_LOG=warn,wallhack=trace,netstack=trace ip netns exec "$NS_ENTRY" "$WALLHACK_BIN" -l ":${WALLHACK_PORT}" --debug </dev/null 2>&1 \
    | sed -u 's/\x1b\[[0-9;]*m//g' > "${LOG_DIR}/diag-entry.log" &
ENTRY_PID=$!
sleep 1

echo "[5/8] Starting wallhack EXIT node..."
RUST_LOG=warn,wallhack=trace ip netns exec "$NS_EXIT" "$WALLHACK_BIN" -c "${IP_ENTRY_EXIT_SIDE}:${WALLHACK_PORT}" -i "$EXIT_ID" --debug </dev/null 2>&1 \
    | sed -u 's/\x1b\[[0-9;]*m//g' > "${LOG_DIR}/diag-exit.log" &
EXIT_PID=$!

echo "[*] Waiting for TUN interface..."
for i in {1..30}; do
    if ip netns exec "$NS_ENTRY" ip link show "$TUN_NAME" >/dev/null 2>&1; then
        echo "    TUN appeared after ${i}s"
        break
    fi
    sleep 1
done

if ! ip netns exec "$NS_ENTRY" ip link show "$TUN_NAME" >/dev/null 2>&1; then
    echo "ERROR: TUN interface did not appear!"
    cat "${LOG_DIR}/diag-entry.log"
    exit 1
fi

echo "[6/8] Configuring TUN interface..."
ip netns exec "$NS_ENTRY" ip link set "$TUN_NAME" up
# Device route only - NO IP assignment on TUN (dumb pipe approach)
ip netns exec "$NS_ENTRY" ip route add 10.200.2.0/24 dev "$TUN_NAME"
echo "    TUN configured: route 10.200.2.0/24 dev $TUN_NAME"

sleep 1

echo "[7/8] Network state..."
echo "--- Client routes ---"
ip netns exec "$NS_CLIENT" ip route
echo "--- Entry routes ---"
ip netns exec "$NS_ENTRY" ip route
echo

echo "[8/8] Testing TCP connection from client through tunnel..."

# Start tcpdumps
ip netns exec "$NS_CLIENT" timeout 15 tcpdump -i veth-ce-client -n tcp port "$ECHO_PORT" > "${LOG_DIR}/diag-client-tcpdump.log" 2>&1 &
ip netns exec "$NS_ENTRY" timeout 15 tcpdump -i "$TUN_NAME" -n > "${LOG_DIR}/diag-tun-tcpdump.log" 2>&1 &
ip netns exec "$NS_ENTRY" timeout 15 tcpdump -i veth-ce-entry -n tcp port "$ECHO_PORT" > "${LOG_DIR}/diag-entry-veth-tcpdump.log" 2>&1 &
sleep 1

echo "=== Attempting TCP connection from client to ${IP_TARGET}:${ECHO_PORT} ==="
ip netns exec "$NS_CLIENT" timeout 10 python3 -c "
import socket
import sys

print(f'Creating socket...', flush=True)
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(5)

print(f'Connecting to ${IP_TARGET}:${ECHO_PORT}...', flush=True)
try:
    s.connect(('${IP_TARGET}', ${ECHO_PORT}))
    print('Connected!', flush=True)
    
    print('Sending: hello', flush=True)
    s.sendall(b'hello')
    
    print('Receiving...', flush=True)
    data = s.recv(1024)
    print(f'Received: {data}', flush=True)
    
    s.close()
    print('SUCCESS: TCP echo test passed!')
    sys.exit(0)
except Exception as e:
    print(f'FAILED: {e}', flush=True)
    sys.exit(1)
" 2>&1
TEST_RESULT=$?

if [[ $TEST_RESULT -eq 0 ]]; then
    echo
    echo "=== TCP TEST PASSED ==="
    echo
    echo "[9/9] Testing UDP connection from client through tunnel..."
    
    # Start a simple UDP echo server in target (the Python echo server is TCP only)
    ip netns exec "$NS_TARGET" timeout 15 python3 -c "
import socket
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind(('0.0.0.0', 9998))
sock.settimeout(10)
print('UDP echo server listening on port 9998', flush=True)
try:
    data, addr = sock.recvfrom(1024)
    print(f'Received from {addr}: {data}', flush=True)
    sock.sendto(data, addr)
    print('Sent response', flush=True)
except socket.timeout:
    print('No UDP packet received')
" &
    UDP_ECHO_PID=$!
    sleep 1
    
    ip netns exec "$NS_CLIENT" timeout 10 python3 -c "
import socket
import sys

print(f'Creating UDP socket...', flush=True)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(5)

print(f'Sending UDP to ${IP_TARGET}:9998...', flush=True)
try:
    s.sendto(b'hello-udp', ('${IP_TARGET}', 9998))
    print('Sent!', flush=True)
    
    print('Receiving...', flush=True)
    data, addr = s.recvfrom(1024)
    print(f'Received: {data} from {addr}', flush=True)
    
    s.close()
    print('SUCCESS: UDP echo test passed!')
    sys.exit(0)
except Exception as e:
    print(f'FAILED: {e}', flush=True)
    sys.exit(1)
" 2>&1
    UDP_RESULT=$?
    
    kill $UDP_ECHO_PID 2>/dev/null || true
    
    if [[ $UDP_RESULT -eq 0 ]]; then
        echo
        echo "=== UDP TEST PASSED ==="
        TEST_RESULT=0
    else
        echo
        echo "=== UDP TEST FAILED ==="
        TEST_RESULT=1
    fi
else
    echo
    echo "=== TCP TEST FAILED ==="
fi

echo
echo "=== Diagnostic Logs ==="
echo "Entry log: ${LOG_DIR}/diag-entry.log"
echo "Exit log:  ${LOG_DIR}/diag-exit.log"

if [[ $TEST_RESULT -eq 0 ]]; then
    echo
    echo "=== ALL TESTS PASSED ==="
else
    echo
    echo "=== TESTS FAILED ==="
    echo
    echo "--- Client veth tcpdump ---"
    cat "${LOG_DIR}/diag-client-tcpdump.log" 2>/dev/null || echo "(none)"
    echo
    echo "--- Entry veth tcpdump ---"
    cat "${LOG_DIR}/diag-entry-veth-tcpdump.log" 2>/dev/null || echo "(none)"
    echo
    echo "--- TUN tcpdump ---"
    cat "${LOG_DIR}/diag-tun-tcpdump.log" 2>/dev/null || echo "(none)"
    echo
    echo "--- Last 30 lines of Entry log ---"
    tail -30 "${LOG_DIR}/diag-entry.log"
fi

exit $TEST_RESULT
