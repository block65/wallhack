"""Network topology constants for wallhack benchmarks.

4-namespace topology:
  wh-client (10.200.0.10) <--veth--> wh-entry (10.200.0.1 + TUN)
  wh-entry (10.200.1.10) <--veth--> wh-exit (10.200.1.20)
  wh-exit (10.200.2.20) <--veth--> wh-target (10.200.2.10)

Traffic flow: client -> entry TUN -> smoltcp -> QUIC -> exit -> target
"""

# Namespace names
NS_CLIENT = "wh-client"
NS_ENTRY = "wh-entry"
NS_EXIT = "wh-exit"
NS_TARGET = "wh-target"

# Veth pair: client <-> entry
VETH_CE_CLIENT = "veth-ce-client"
VETH_CE_ENTRY = "veth-ce-entry"

# Veth pair: entry <-> exit
VETH_EE_ENTRY = "veth-ee-entry"
VETH_EE_EXIT = "veth-ee-exit"

# Veth pair: exit <-> target
VETH_ET_EXIT = "veth-et-exit"
VETH_ET_TARGET = "veth-et-target"

# IP addresses
IP_CLIENT = "10.200.0.10"
IP_ENTRY_CLIENT_SIDE = "10.200.0.1"
IP_ENTRY_EXIT_SIDE = "10.200.1.10"
IP_EXIT_ENTRY_SIDE = "10.200.1.20"
IP_EXIT_TARGET_SIDE = "10.200.2.20"
IP_TARGET = "10.200.2.10"

# Subnets
SUBNET_CLIENT_ENTRY = "10.200.0.0/24"
SUBNET_ENTRY_EXIT = "10.200.1.0/24"
SUBNET_EXIT_TARGET = "10.200.2.0/24"

# Prefix lengths
PREFIX_LEN = 24

# Wallhack settings
WALLHACK_LISTEN_PORT = 6565
EXIT_ID = "bench"
TUN_NAME = f"tun-{EXIT_ID}"

# Echo server
ECHO_PORT = 9999

# iperf3
IPERF3_PORT = 5201

# Timeouts (seconds)
TUN_READY_TIMEOUT = 30
TUN_POLL_INTERVAL = 0.5
PROCESS_STARTUP_DELAY = 1.0
