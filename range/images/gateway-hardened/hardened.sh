#!/bin/sh

# =============================================================================
# HARDENED GATEWAY - Egress only, no unsolicited inbound
# =============================================================================
# This simulates a real corporate firewall:
# - Outbound connections: ALLOWED
# - Inbound replies to outbound: ALLOWED (ESTABLISHED,RELATED)
# - Unsolicited inbound: BLOCKED (except SSH for management)
#
# To get past this, you need REVERSE tunnels - connect OUT from the inside.
# =============================================================================

iptables -F
iptables -t nat -F

# Default policies
iptables -P INPUT DROP
iptables -P FORWARD DROP
iptables -P OUTPUT ACCEPT

# Loopback
iptables -A INPUT -i lo -j ACCEPT

# Allow established/related replies
iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT

# SSH in (you still need to reach this box)
iptables -A INPUT -p tcp --dport 22 -j ACCEPT

# FORWARD chain - this is where the magic happens
# Only allow packets that are replies to connections initiated from the "inside"
iptables -A FORWARD -m state --state ESTABLISHED,RELATED -j ACCEPT

# Allow NEW connections going OUTBOUND (from management to datacenter)
# eth0 = datacenter (10.99.3.0/24), eth1 = management (10.99.4.0/24)
# Connections FROM management TO datacenter = allowed (reverse tunnel direction)
iptables -A FORWARD -i eth1 -o eth0 -j ACCEPT

# Block NEW connections going INBOUND (from datacenter to management)
# This is implicit due to DROP policy, but let's be explicit
iptables -A FORWARD -i eth0 -o eth1 -m state --state NEW -j DROP

# Log dropped packets (useful for debugging)
iptables -A FORWARD -j LOG --log-prefix "HARDENED-DROP: " --log-level 4

echo "[*] Hardened firewall active"
echo "[*] Inbound to management network: BLOCKED"
echo "[*] Outbound from management network: ALLOWED"
echo "[*] Reverse tunnels required to reach vault"

# Start SSH
exec /usr/sbin/sshd -D -e
