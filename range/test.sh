#!/bin/bash
set -e

COMPOSE_PROJECT="wallhack-range"

# Network and service configuration
SSH_PORT=2222
TIMEOUT_SECONDS=5
CURL_TIMEOUT=2
NC_TIMEOUT=2
PING_COUNT=1
PING_TIMEOUT=1

# IP addresses
IP_WEB_EXTERNAL="10.99.1.80"
IP_FTP_SERVER="10.99.1.21"
IP_CORP_PROXY="10.99.1.50"
PORT_CORP_PROXY=3128
IP_CORP_SOCKS="10.99.1.51"
PORT_CORP_SOCKS=1080
IP_GATEWAY_OFFICE="10.99.2.10"
IP_GATEWAY_DATACENTER="10.99.3.10"
IP_GATEWAY_MANAGEMENT="10.99.4.10"
IP_SECRETS_SERVER="10.99.5.10"
IP_VAULT="10.99.5.100"

# Ports
PORT_VAULT=5000

compose() {
  docker compose -p "$COMPOSE_PROJECT" "$@"
}

container() {
  echo "${COMPOSE_PROJECT}-${1}-1"
}

# --- helpers ----------------------------------------------------------------

PASS=0
FAIL=0

ok()   { printf '\033[1;32m  PASS: %s\033[0m\n' "$*"; PASS=$((PASS + 1)); }
err()  { printf '\033[1;31m  FAIL: %s\033[0m\n' "$*"; FAIL=$((FAIL + 1)); }
section() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }

# --- container health --------------------------------------------------------

section "Container health"

RUNNING=$(compose ps --status running --format json | jq -s 'length')
if [ "$RUNNING" -gt 0 ]; then
  ok "$RUNNING containers running"
else
  err "No containers running"
  compose ps
  exit 1
fi

# --- SSH entry point ---------------------------------------------------------

section "SSH entry point"

if timeout "$TIMEOUT_SECONDS" nc -z localhost "$SSH_PORT" 2>/dev/null; then
  ok "SSH port $SSH_PORT accessible"
else
  err "SSH port $SSH_PORT not accessible"
  exit 1
fi

# --- perimeter services ------------------------------------------------------

section "Perimeter services"

if docker exec "$(container gateway-perimeter)" curl -s -m "$CURL_TIMEOUT" http://${IP_WEB_EXTERNAL} >/dev/null; then
  ok "web-external (${IP_WEB_EXTERNAL}:80)"
else
  err "web-external not accessible"
fi

if docker exec "$(container gateway-perimeter)" nc -z -w "$NC_TIMEOUT" "$IP_FTP_SERVER" 21 2>/dev/null; then
  ok "ftp-server (${IP_FTP_SERVER}:21)"
else
  err "ftp-server not accessible"
fi

# --- corp proxy --------------------------------------------------------------

section "Corp proxy"

if docker exec "$(container attacker)" nc -z -w "$NC_TIMEOUT" "$IP_CORP_PROXY" "$PORT_CORP_PROXY" 2>/dev/null; then
  ok "corp-proxy (${IP_CORP_PROXY}:${PORT_CORP_PROXY}) reachable from attacker"
else
  err "corp-proxy not reachable from attacker"
fi

if docker exec "$(container attacker)" curl -s -m "$CURL_TIMEOUT" \
    --proxy "http://${IP_CORP_PROXY}:${PORT_CORP_PROXY}" \
    "http://${IP_WEB_EXTERNAL}" >/dev/null 2>&1; then
  ok "HTTP CONNECT through corp-proxy -> web-external works"
else
  err "HTTP CONNECT through corp-proxy failed"
fi

section "Corp SOCKS5 proxy"

if docker exec "$(container attacker)" nc -z -w "$NC_TIMEOUT" "$IP_CORP_SOCKS" "$PORT_CORP_SOCKS" 2>/dev/null; then
  ok "corp-socks (${IP_CORP_SOCKS}:${PORT_CORP_SOCKS}) reachable from attacker"
else
  err "corp-socks not reachable from attacker"
fi

if docker exec "$(container attacker)" curl -s -m "$CURL_TIMEOUT" \
    --proxy "socks5://${IP_CORP_SOCKS}:${PORT_CORP_SOCKS}" \
    "http://${IP_WEB_EXTERNAL}" >/dev/null 2>&1; then
  ok "SOCKS5 through corp-socks -> web-external works"
else
  err "SOCKS5 through corp-socks failed"
fi

# --- pivot path --------------------------------------------------------------

section "Pivot path"

declare -A PIVOTS=(
  ["gateway-perimeter:${IP_GATEWAY_OFFICE}"]="gateway-office"
  ["gateway-office:${IP_GATEWAY_DATACENTER}"]="gateway-datacenter"
  ["gateway-datacenter:${IP_GATEWAY_MANAGEMENT}"]="gateway-management"
  ["gateway-management:${IP_SECRETS_SERVER}"]="secrets-server"
)

for key in "${!PIVOTS[@]}"; do
  IFS=':' read -r from ip <<< "$key"
  to="${PIVOTS[$key]}"
  if docker exec "$(container "$from")" ping -c "$PING_COUNT" -W "$PING_TIMEOUT" "$ip" >/dev/null 2>&1; then
    ok "$from -> $to ($ip)"
  else
    # Fall back to SSH check
    if docker exec "$(container "$from")" nc -z -w "$NC_TIMEOUT" "$ip" 22 >/dev/null 2>&1; then
      ok "$from -> $to ($ip) via SSH"
    else
      err "$from cannot reach $to ($ip)"
    fi
  fi
done

# --- hardened firewall -------------------------------------------------------

section "Hardened firewall (gateway-management)"

if docker exec "$(container gateway-management)" iptables -L FORWARD -n 2>/dev/null | grep -q "DROP"; then
  ok "Hardened firewall rules active"

  # Verify egress works (management -> vault)
  if docker exec "$(container gateway-management)" curl -s -m "$CURL_TIMEOUT" http://${IP_VAULT}:${PORT_VAULT} >/dev/null 2>&1; then
    ok "Outbound: management -> vault works"
  else
    err "Outbound to vault not working"
  fi

  # Verify ingress blocked (datacenter -> vault through management)
  if docker exec "$(container gateway-datacenter)" nc -z -w "$NC_TIMEOUT" "$IP_VAULT" "$PORT_VAULT" 2>/dev/null; then
    err "Inbound NOT blocked - forward to vault worked (firewall misconfigured)"
  else
    ok "Inbound: datacenter -> vault blocked (reverse tunnel required)"
  fi
else
  err "Hardened firewall not detected"
fi

# --- summary -----------------------------------------------------------------

section "Summary"
echo ""
echo "  Passed: $PASS"
echo "  Failed: $FAIL"
echo ""
if [ "$FAIL" -eq 0 ]; then
  echo "  ALL PASSED"
else
  echo "  $FAIL FAILURES"
fi

exit "$FAIL"
