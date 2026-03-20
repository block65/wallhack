# wallhack Feature Catalogue

A comprehensive inventory of every feature in wallhack, documented for
penetration testers, red teamers, and CTF players.

Layer 3 tunneling over QUIC and WebSockets, written in Rust, purpose-built for
network pivoting and penetration testing. Statically linked musl binaries, no
runtime dependencies. The name is intentional: it makes the network transparent.

---

## Transport Layer

### Dual-Transport Architecture
**Where:** `crates/transport/`, `crates/core/src/server/`, `crates/core/src/client/`

wallhack supports two transport protocols, selectable per-connection via a
Docker-style address suffix (`/tcp`, `/udp`):

- **QUIC (UDP)** — Default. Uses quinn over UDP. Sub-millisecond RTT
  (benchmarked at 0.065ms min, 0.195ms avg). Handles packet loss gracefully
  with built-in congestion control. Up to 10,000 concurrent bidirectional
  streams per connection.

- **WebSocket over TLS (TCP)** — For restrictive networks. Uses
  tokio-tungstenite with yamux multiplexing on top, giving it QUIC-like stream
  semantics over a single TCP connection. Traverses HTTP proxies and CDNs that
  block UDP.

Both transports share an identical `Transport` trait (`crates/transport/src/traits.rs`)
so the entire data plane is transport-agnostic. You can mix — e.g., QUIC
between entry and relay, WebSocket between relay and exit behind a corporate
proxy.

### Proxy Traversal (WebSocket mode)
**Where:** `crates/core/src/client/ws/mod.rs:105-166`

The WebSocket client auto-detects proxy configuration from environment
variables following curl conventions:

- **SOCKS5** — `socks5://` and `socks5h://` (remote DNS) via `tokio-socks`
- **HTTP CONNECT** — `http://` and `https://` proxy URLs via `async-http-proxy`
- **NO_PROXY** — Respects comma-separated bypass list with wildcard and domain
  suffix matching
- **Credentials** — Strips `user:pass@` from proxy URLs

This means wallhack tunnels work through corporate proxies and SOCKS5 gateways
without any additional tooling.

### Domain Fronting
**Where:** `crates/core/src/client/ws/mod.rs:177-182`

The WebSocket client config has a `host_header` field that overrides the HTTP
Host header independently from the TLS SNI. This enables domain fronting through
CDNs — connect to a CDN IP with the CDN's domain in SNI but your C2 domain in
the Host header.

```rust
pub struct WsClientConfig {
    pub host_header: Option<String>,  // Override host header (for CDN fronting)
    pub use_tls: bool,                // wss:// vs ws://
    pub path: String,                 // WebSocket path (e.g., "/ws")
    // ...
}
```

---

## Authentication & Cryptography

### PSK with TLS Channel Binding
**Where:** `crates/core/src/psk.rs`, `crates/core/src/hmac.rs`

Pre-shared key authentication that never transmits the key. The proof is an
HMAC-SHA256 over:
1. The serialized handshake (name, version, capabilities, routes, hints)
2. TLS exported keying material (RFC 9266 `tls-exporter` channel binding)

This means the PSK proof is:
- **Session-bound** — replay from a different TLS session is rejected
- **Content-bound** — tampering with the handshake invalidates the proof
- **Constant-time verified** — via `ring::hmac::verify`

The PSK is wrapped in `zeroize::Zeroizing<String>` so it's scrubbed from memory
on drop.

### Certificate Fingerprint Pinning (TOFU)
**Where:** `crates/core/src/tls/verifiers.rs:62-132`

A custom `rustls::ServerCertVerifier` that validates the server certificate by
its SHA-256 fingerprint. Connect once, grab the fingerprint, pin it for future
connections — trust-on-first-use model.

```
--accept-fingerprint sha256:a1b2c3...
```

If no fingerprint and no mTLS is configured, the client uses a
`SkipServerVerification` verifier — explicitly dangerous, but convenient for
lab environments and CTFs.

### Mutual TLS (mTLS)
**Where:** `crates/core/src/client/tls_config.rs:40-70`, `crates/core/src/server/tls.rs:46-59`

Full mTLS support with CA root loading from PEM or DER files. The server can
require client certificates, and the client can present its own cert/key pair.

### Self-Signed Certificate Generation
**Where:** `crates/core/src/server/tls.rs:90-96`

When no TLS config is provided, wallhack generates an ephemeral self-signed
certificate using `rcgen` at startup. Zero-config deployment — just run it.

---

## Network Architecture

### Multi-Role Node System
**Where:** `crates/core/src/types.rs`, `crates/daemon/src/mode/`

Four node roles with clean separation:

| Role | What it does | TUN? |
|------|-------------|------|
| **Entry** | Creates TUN interface, intercepts traffic, routes it through the tunnel | Yes |
| **Exit** | Receives tunneled instructions, makes real network syscalls | No |
| **Relay** | Forwards messages between entry and exit without processing | No |
| **Indeterminate** | Waiting for negotiation to resolve | N/A |

### Automatic Role Negotiation
**Where:** `crates/core/src/negotiate.rs`

This is genuinely elegant. Both peers independently derive the same topology
from a pure function — no coordinator, no leader election, no I/O:

```
negotiate(local_handshake, peer_handshake) -> NegotiationResult
```

Rules (priority order):
1. **FIXED hint** — override everything
2. **Capability-based** — TUN capability + listen/connect direction
3. **EXCLUDE hint** — remove a role from consideration
4. **PREFER hint** — break ambiguity

Both sides call the same function with swapped arguments and arrive at
complementary roles. A TUN-capable listener paired with a non-TUN connector
always resolves to entry/exit without any hints needed.

### Role Hints (Runtime Re-negotiation)
**Where:** `crates/core/src/control/handler.rs:496-504`, `crates/cli/src/repl.rs:193-239`

Operators can adjust roles at runtime via the REPL or CLI:

```
hint fixed entry     # Force this node to entry
hint prefer exit     # Suggest exit, but allow override
hint exclude relay   # "I refuse to be relay"
hint clear           # Reset all hints
role entry           # Shorthand for "hint fixed entry"
```

The hint is pushed through a `watch` channel to the mode task, which
re-evaluates the negotiation on the next connection.

### Relay Chain Architecture
**Where:** `crates/daemon/src/mode/relay.rs`

Relay nodes connect to a source peer (entry/relay) and listen for exit nodes,
forwarding messages between them with a fan-out task that distributes
instructions to all connected exit peers. When the source connection drops,
the relay tears down its listener and reconnects — exit peers reconnect via
their own retry loops.

This enables multi-hop chains: `entry ← relay ← relay ← exit`, with each
link using potentially different transports.

### Auto-Managed Route Advertisement
**Where:** `crates/daemon/src/mode/auto.rs:131-196`, `crates/daemon/src/netlink.rs:199-295`

Exit nodes enumerate their local network interfaces via Netlink
(`RTM_GETADDR`), mask to network addresses, filter out loopback/link-local/
multicast, and advertise the resulting CIDRs in their `Handshake.routes` field.

When an entry node sees these routes, it:
1. Adds them to the route table as auto-managed
2. Installs OS-level routes via Netlink (`RTM_NEWROUTE`) pointing at the TUN
3. Automatically removes them (both table and OS) when the peer disconnects

No manual `route add` needed — plug in an exit node and traffic flows.

---

## Userspace TCP/IP Stack

### smoltcp-Based Entry Stack
**Where:** `crates/entry-stack/`, `crates/core/src/entry/actor.rs`

The entry node runs a full userspace TCP/IP stack (smoltcp) on its TUN
interface. This is not just a packet forwarder — it's a complete TCP
implementation that:

- Handles TCP handshakes (SYN/SYN-ACK/ACK)
- Manages TCP state machines (ESTABLISHED, FIN-WAIT, TIME-WAIT, etc.)
- Processes UDP datagrams with session tracking
- Computes IP, TCP, UDP, and ICMP checksums
- Supports IPv4 and IPv6

### AnyIP Mode
**Where:** `crates/core/src/entry/actor.rs:48-58`

The TUN interface is configured with `0.0.0.0/0` and `any_ip: true`, which
means smoltcp accepts connections to *any* IP address. Point any subnet at the
TUN interface and it transparently proxies everything. No per-destination
configuration needed.

### SYN Proxy with Port Probing
**Where:** `crates/core/src/entry/syn_proxy.rs`, `crates/entry-stack/`

This is where things get really interesting for pentesters. When a TCP SYN
arrives for an unknown (host, port):

1. The entry stack **holds the SYN** (doesn't complete the handshake yet)
2. Opens a probe bi-stream to the exit node
3. Exit node attempts the real TCP connect
4. Based on the result:
   - **Open** → cache result, inject original SYN back, smoltcp completes handshake
   - **Closed** (ECONNREFUSED) → cache, inject, smoltcp RSTs the client
   - **Unreachable** (EHOSTUNREACH) → inject ICMP Host Unreachable into the TUN

Results are cached in `SynProbeCache` so subsequent SYNs to the same
(host, port) resolve instantly.

**Why this matters for nmap:** Without the SYN proxy, smoltcp would SYN-ACK
every connection attempt (because AnyIP), making every port appear "open".
The SYN proxy gives nmap accurate open/closed/filtered responses through the
tunnel.

### JIT Socket Binding
**Where:** `crates/entry-stack/src/inner/` (peek_device, tcp_listener_any)

The entry stack uses a "peek before poll" pattern: it reads all pending packets
from the TUN device *before* processing them, examines destination ports, and
creates TCP listener sockets just-in-time. This handles burst SYN scenarios
(like port scans) where many SYNs arrive simultaneously for different ports.

### ICMP Tunneling
**Where:** `crates/core/src/entry/icmp.rs`, `crates/core/src/entry/manager.rs:430-497`, `crates/exit-adapter/src/sessions/icmp.rs`

Full ICMP echo request/reply tunneling:

1. Entry intercepts ICMP Echo Request from the TUN
2. Parses the raw IP packet, extracts ident/seq/data
3. Sends as `IcmpSendInstruction` through the tunnel
4. Exit node opens a raw DGRAM ICMP socket (`socket2`), sends the echo request
5. Waits for reply (5s timeout)
6. Sends the raw ICMP reply back through the tunnel
7. Entry reconstructs a full IP+ICMP packet with the *original* identifier
   (the OS on the exit node may substitute its own)
8. Injects the reply into the TUN

**Result:** `ping` works through the tunnel, with correct latency measurements.

### ICMP Error Injection
**Where:** `crates/core/src/entry/icmp.rs:14-199`

When the exit node reports UDP errors (ECONNREFUSED, EHOSTUNREACH,
ENETUNREACH), the entry node constructs proper ICMP Destination Unreachable
packets (both IPv4 and IPv6 variants) and injects them into the TUN.

This gives tools like nmap accurate host-down and port-unreachable feedback
through the tunnel, rather than silent drops.

---

## Data Plane

### Bidirectional TCP Relay
**Where:** `crates/core/src/entry/session.rs`, `crates/daemon/src/mode/exit.rs:564-673`

TCP connections use `tokio::io::copy_bidirectional_with_sizes` with 64KB buffers
for zero-copy-ish bidirectional streaming. The exit node:

1. Receives `TcpStreamHeader` with target address on a QUIC/yamux bi-stream
2. Connects to the target (with retry for transient EHOSTUNREACH)
3. Sends `TcpStreamStatus` (Success/ConnectionRefused/HostUnreachable) back
4. If successful, enters bidirectional copy mode until either side closes

The entry side waits for the success confirmation before completing the TCP
handshake back to the client — so the client never sees a successful connect
for an unreachable target.

### UDP Session Management
**Where:** `crates/core/src/entry/manager.rs:139-194`, `crates/core/src/exit/orchestrator.rs:345-444`

UDP sessions are tracked by `(source_endpoint, local_port)` pairs with a 30s
idle timeout. Each unique UDP flow spawns a receive task on the exit node that
forwards responses back through the tunnel.

### Protobuf Wire Protocol
**Where:** `crates/wire/` (generated from .proto), `crates/core/src/transport/protocol.rs`

All tunnel communication uses length-delimited protobuf messages. The protocol
has three stream types:

1. **Control bidi-stream** — Persistent. Carries handshakes, ping/pong, control
   requests/responses, disconnect signals, role transitions
2. **Data uni-stream (entry→exit)** — Instructions: TcpConnect, TcpSend,
   UdpSend, IcmpSend, TcpListen, etc.
3. **Data uni-stream (exit→entry)** — Responses: TcpResponse, UdpResponse,
   IcmpResponse, RuntimeError, etc.

### Ping/Pong Latency Measurement
**Where:** `crates/core/src/transport/protocol.rs:201-264`

The control stream runs a periodic ping timer. Each ping carries a millisecond
timestamp; the pong echoes it back. The receiver computes RTT from
`now - pong.timestamp_ms` and feeds it to the peer registry for display in
`wallhack peers`.

### Session Reaping
**Where:** `crates/exit-adapter/`

The exit adapter runs a background reaper task that periodically scans for idle
sessions (TCP, UDP, ICMP) and closes them. Default: check every 1 minute, reap
sessions idle for 5 minutes. This prevents resource leaks from abandoned
connections.

---

## Control Plane & Management

### Unix Socket IPC
**Where:** `crates/core/src/ipc.rs`, `crates/ipc/src/client.rs`

The daemon exposes a Unix domain socket (like Docker) for management:

- Default path: `$XDG_RUNTIME_DIR/wallhack/wallhackd.sock`
- Override: `WALLHACK_HOST=unix:///path/to/sock` or `WALLHACK_HOST=/path`
- Fallback chain: XDG → `/tmp/wallhack-$USER/` → `$HOME/.wallhack/` → `/tmp/wallhack-shared/`

Protocol: length-delimited protobuf `ManagementRequest`/`DaemonMessage` frames.
The connection also pushes real-time `DaemonNotification` events (peer
connected/disconnected) alongside request-response traffic.

### vsock IPC (VM Guest Support)
**Where:** `crates/core/src/ipc.rs:125-174`, `crates/ipc/src/client.rs:37-41,146-186`

When compiled with the `vsock` feature, the daemon also listens on
`VMADDR_CID_ANY:4434` for virtio-vsock connections. This means wallhack
running inside a VM (e.g., a microVM lab environment) can be controlled from
the hypervisor host without any network connectivity.

```
WALLHACK_HOST=vsock://3:4434 wallhack peers
```

### Interactive REPL
**Where:** `crates/cli/src/repl.rs`

Full interactive shell with `reedline` for line editing and persistent history
(~/.wallhack_history). Commands:

```
ping [peer]                      Ping a peer
info                             Show daemon info
stats                            Show traffic statistics
peers                            List connected peers
route list                       List configured routes
route add <cidr> [via] <peer>    Add a route
route del <cidr>                 Remove a route
connect <addr>                   Connect to a peer
listen <addr>                    Start listening
disconnect [peer]                Disconnect peer
role                             Show current role
role <entry|exit|relay>          Set role hint
hint <prefer|exclude|fixed> <r>  Apply a role hint
hint clear                       Clear all hints
shutdown                         Shut down the daemon
```

### CLI Control Client
**Where:** `crates/cli/src/cli.rs`

One-shot commands for scripting and automation:

```bash
wallhack ping
wallhack peers --json
wallhack route add 10.0.0.0/8 --peer exit-1
wallhack stats
wallhack shutdown
```

Supports `-H` flag and `WALLHACK_HOST` for remote daemon control.

### REST API
**Where:** `crates/api/`

HTTPS REST API (axum) for programmatic control of entry nodes. Endpoints:

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check (public) |
| GET | `/ping` | Ping daemon |
| GET | `/stats` | Traffic statistics |
| GET | `/peers` | List peers |
| DELETE | `/peers/{name}` | Disconnect peer |
| GET | `/routes` | List routes |
| POST | `/routes` | Add route |
| DELETE | `/routes/{cidr}` | Remove route |

Security features:
- HTTP Basic Auth with configurable credentials
- DNS rebinding protection (Host header validation)
- Security headers: CSP, X-Frame-Options, X-Content-Type-Options, no-cache,
  no-referrer, Permissions-Policy
- Uses the same TLS certificates as the tunnel server
- OpenAPI spec at `website/src/data/openapi.json`

### MCP Server (AI Agent Control)
**Where:** `crates/mcp/`

An MCP (Model Context Protocol) server that exposes the full management API as
AI-callable tools. Claude Code (or any MCP client) can directly manage wallhack
nodes:

- `status`, `ping`, `stats`, `peers`, `routes`
- `add_route`, `remove_route`
- `connect`, `listen`, `disconnect`, `disconnect_peer`
- `shutdown`

Each tool call opens a fresh IPC connection and returns formatted text. This
means an AI agent can orchestrate a multi-node wallhack deployment.

---

## Peer Management

### Wait-Free Peer Registry
**Where:** `crates/core/src/control/peers.rs`

The peer registry uses `ArcSwap` for wait-free reads — no mutexes on the hot
path. Each peer tracks:

- Name, address, role, capabilities
- Connection side (who initiated)
- Connected timestamp
- Total bytes transferred
- Latency (ms) with staleness detection
- TUN interface name (entry-side)

### Peer Event System
**Where:** `crates/core/src/control/peers.rs:17-27`

Broadcast channel for peer lifecycle events (`Connected`, `Disconnected`). The
IPC layer subscribes and pushes notifications to connected clients in real-time.

### Peer Name Prefix Resolution
**Where:** `crates/core/src/control/peers.rs:329-356`

All peer-targeting commands accept unambiguous name prefixes:

```
disconnect gw    # Disconnects "gateway-perimeter" if it's the only "gw..." peer
```

Returns an error if the prefix is ambiguous (lists matching names).

### Max Peers Limit
**Where:** `crates/daemon/src/mode/entry.rs:642-644`

Entry nodes can cap concurrent connections with `--max-peers`. Uses a tokio
Semaphore, so connections beyond the limit are rejected immediately.

### PSK Failure Tracking
**Where:** `crates/daemon/src/mode/` (PskFailTracker)

Failed PSK authentication attempts are logged with the offending peer address.
Prevents log spam from repeated brute-force attempts.

---

## Route Management

### Wait-Free Route Table
**Where:** `crates/core/src/control/routes.rs`

CIDR-to-peer mapping with `ArcSwap` for wait-free reads. Supports:

- Manual routes (persist across reconnections)
- Auto-managed routes (installed from peer handshake, removed on disconnect)
- Route update broadcast channel for live TUN route synchronization
- `remove_by_peer` for bulk cleanup on disconnect

### OS Route Integration via Netlink
**Where:** `crates/daemon/src/netlink.rs`

Routes are installed directly into the Linux routing table via Netlink
(`RTM_NEWROUTE`, `RTM_DELROUTE`) — no subprocess spawning, no `ip route add`.
Handles EEXIST (idempotent add) and ESRCH (idempotent remove) gracefully.

### Live Route Updates
**Where:** `crates/daemon/src/mode/entry.rs:549-571`

A background task watches the route update broadcast channel. When routes are
added or removed (via REPL, CLI, REST API, or auto-management), the
corresponding OS routes are immediately installed or removed on the TUN
interface.

---

## Daemon Engine

### Structured DaemonConfig
**Where:** `crates/daemon/src/daemon_config.rs`

The daemon library is decoupled from CLI parsing. The CLI builds a
`DaemonConfig` and passes it in — making it embeddable in other applications.

### TUN Capability Detection
**Where:** `crates/daemon/src/tun_cap.rs`

Probes `/dev/net/tun` with read+write access — one syscall, correct answer for
non-root users with `CAP_NET_ADMIN` and root inside containers that lack the
capability. No `geteuid()` heuristics.

### Entropy Pool Check
**Where:** `crates/daemon/src/sys.rs`

On Linux, checks if `/dev/random` is non-blocking before starting. Warns if
the entropy pool isn't seeded yet (relevant for early-boot scenarios in VMs).

### Automatic Reconnection
**Where:** `crates/daemon/src/transport.rs` (connect_loop)

All connect-mode nodes use a retry loop with backoff. When a connection drops,
the node automatically reconnects. The entry node's TUN interface persists
across reconnections — sessions using the same exit peer get the same TUN name
(via FNV-1a hash of peer name).

### Stable TUN Interface Names
**Where:** `crates/daemon/src/mode/entry.rs:40-46`

TUN names are derived from peer names via FNV-1a hash:
`peer_name_to_iface("gateway-perimeter") → "wh4a3b7c2d"`. Always 10 chars
(within Linux's IFNAMSIZ), deterministic, unique per peer. Reconnecting exit
nodes get the same TUN interface.

### Docker-Style Address Parsing
**Where:** `crates/daemon/src/address_spec.rs`

Addresses use a `host:port/protocol` format: `10.99.1.100:443/udp`,
`proxy.corp:8080/tcp`. Default port (6565) is auto-applied. Default protocol
is UDP.

### DNS Resolution
**Where:** `crates/daemon/src/dns/`

Exit and relay nodes resolve hostnames before connecting, with an optional
`--dns-server` override.

### Local CIDR Enumeration
**Where:** `crates/daemon/src/netlink.rs:199-295`

Queries the kernel via `RTM_GETADDR` to discover all globally-routable CIDRs
on local interfaces. Filters out loopback, link-local, unspecified, and
multicast. Used for handshake route advertisement.

---

## Build & Deployment

### Multi-Call Binary
**Where:** `crates/cli/src/bin/wallhack.rs`

Single binary that functions as:
- `wallhack` — CLI control client
- `wallhackd` — Daemon launcher

### Slim Build
**Where:** workspace `Cargo.toml`, feature flags

`--no-default-features --features slim` produces a minimal binary with just
QUIC and WebSocket support — no REPL, no HTTP API. For resource-constrained
deployment targets.

### Release Profile
**Where:** workspace `Cargo.toml`

```toml
[profile.release]
strip = true
opt-level = 3
lto = true
panic = "abort"
codegen-units = 1
```

Maximum optimization, stripped symbols, link-time optimization, abort on panic.
The resulting binary is as small and fast as possible.

### Static musl Linking
**Where:** `range/pontoon.yml` build config

The range uses `x86_64-unknown-linux-musl` target for fully static binaries
that run anywhere — no glibc dependency.

### `unsafe` Forbidden
**Where:** workspace `Cargo.toml`

```toml
[workspace.lints.rust]
unsafe_code = "deny"
```

The only exception is the tracking allocator in memory budget tests. The entire
production codebase is safe Rust.

---

## Testing & Quality

### Memory Budget Tests
**Where:** `crates/core/tests/memory_budget.rs`

A custom tracking allocator measures heap usage for every major runtime
component with hard budget assertions:

- **Constrained target** (RPi Zero / t4g.nano): 64 MB budget
- **Moderate target** (RPi 4 / small VPS): 256 MB budget
- Tests: struct sizes, broadcast channel scaling, per-connection overhead,
  filled channel costs, burst peak memory, mpsc costs, tokio runtime overhead
- Prints a formatted budget report on every test run

### PCAP Replay Tests
**Where:** `crates/entry-stack/tests/pcap_replay.rs`

Replays "The Ultimate PCAP" through the entry stack:

- **Robustness test** — feeds every IP packet, asserts no panics
- **Targeted SYN test** — crafts a SYN, verifies SYN-ACK response
- **Full handshake test** — SYN → SYN-ACK → ACK → data → verify recv
- **AnyIP test** — verifies SYN-ACK with 0.0.0.0/0 address
- **JIT binding test** — packet arrives before listener exists, creates
  listener, processes packet

Supports Ethernet, raw IP, BSD loopback, Linux cooked capture v1/v2, and
802.1Q VLAN tags.

### Socket Accumulation Tests
**Where:** `crates/entry-stack/tests/socket_accumulation.rs`

Regression tests for socket leaks: verifies that 1,000 sequential connections
don't accumulate sockets, and that pruning correctly removes closed sockets.

### WebSocket Transport Benchmarks
**Where:** `crates/transport/benches/websocket.rs`

Criterion benchmarks for:
- `WebSocketByteStream/write_64k` — write path framing overhead
- `yamux/stream_open_round_trip` — stream open/accept latency

### Integration Benchmark Suite
**Where:** `bench/`

Python-based benchmarks using network namespaces (entry, exit, client, target):

- Throughput: 0.11, 0.5, 1, 5, 10 Mbps tiers
- Lossy conditions: 0.5% loss + 10ms RTT, 2.0% loss + 50ms RTT
- Parallel streams: 1-5 concurrent TCP streams
- Reverse mode
- TCP echo with payload sizes from 1B to 1MB
- Both QUIC and WebSocket transports
- Memory profiling (peak RSS tracking)

---

## Range / Lab Environment

### Pontoon Virtual Range
**Where:** `range/pontoon.yml`, `range/layers/`, `range/vm/`

A complete enterprise network simulation using Pontoon (microVM orchestrator).
20+ services across 6 network segments:

**Perimeter (10.99.1.0/24)**
- `attacker` — Entry node, 512 MB, 2 CPUs, listens on :443
- `gateway-perimeter` — Exit node, connects to attacker
- `web-external` — External web server
- `web-filter` — Exit node with egress-web-only firewall (HTTP/HTTPS only)
- `ftp-server` — vsftpd
- `corp-proxy` — Squid HTTP proxy (bridges to proxy-vault network)
- `corp-socks` — Dante SOCKS5 proxy

**Office (10.99.2.0/24)** — Internal network
- `ssh-bastion` — SSH jump host with egress-ssh-only firewall
- `loot` — Target app (deny_cp, deny_root)
- `gateway-office` — Routes to datacenter
- `fileserver` — Samba
- `intranet` — Internal web
- `ssh-server` — Hardened SSH
- `printer` — Print server

**Datacenter (10.99.3.0/24)**
- `gateway-datacenter` — Routes to management
- `db-postgres`, `db-mariadb` — Databases
- `redis`, `memcached` — Caches
- `udp-only` — Exit node with egress-udp53-only firewall (DNS only!)
- `api-server` — Internal API

**Management (10.99.4.0/24)**
- `gateway-management` — Routes to vault
- `monitoring` — Prometheus

**Vault (10.99.5.0/24)** — High-security zone
- `reverse-target` — Exit node with egress-none firewall, listens on :9000
  (reverse connect: the entry node must connect to this exit, not the other
  way around)
- `backup-server` — SSH backup
- `gold` — The ultimate target (10.99.5.100)

**Proxy-Vault (10.99.6.0/24)** — Only reachable through corp-proxy
- `platinum` — Hard mode target (deny_root, deny_cp)

### VM Init System
**Where:** `range/vm/init.sh`

Custom BusyBox init script that:
- Mounts proc/sysfs/devtmpfs
- Parses kernel cmdline for network and service configuration
- Configures interfaces, gateways, IP forwarding, masquerade via iptables
- Mounts 9p host share for file injection
- Starts services in background subshells
- Signals `BOOT_COMPLETE_V2` for orchestrator readiness detection
- Spawns shells on both ttyS0 (user console) and hvc0/ttyS1 (MCP agent)
- Sets `ping_group_range 0 2147483647` (allows unprivileged ICMP)

### Layer-Based VM Composition
**Where:** `range/layers/`

Services are composed from layers:
- `base` — Alpine Linux rootfs
- `wallhack` — Injects the wallhack binary
- `attacker` — nmap, curl, and offensive tools
- `perimeter-gw` — IP forwarding + masquerade
- `egress-none` — Drops all outbound traffic
- `egress-ssh-only` — Only allows SSH outbound
- `egress-udp53-only` — Only allows DNS (UDP/53) outbound
- `egress-web-only` — Only allows HTTP/HTTPS outbound
- `proxy-env` — Configures HTTP_PROXY/HTTPS_PROXY environment
- Various app layers (postgres, redis, samba, etc.)

---

## Operational Details

### Metrics
**Where:** `crates/core/src/control/metrics.rs`

Lock-free atomic counters for:
- `bytes_in` / `bytes_out` — Total tunnel bytes
- `packets_in` / `packets_out` — Total tunnel packets
- `active_connections` — Current TCP sessions
- `active_flows` — Current UDP flows
- `packets_dropped` — Backpressure drops

### Tracing
**Where:** `crates/cli/src/subscriber.rs`

Uses the `tracing` crate with CLI-controlled verbosity:
- `--debug [--debug-filter <substr>]` — DEBUG level, optional module filter
- `--trace [--trace-filter <substr>]` — TRACE level, optional module filter

No `RUST_LOG` environment variable — levels are always explicit.

### Graceful Shutdown
**Where:** `crates/core/src/daemon.rs`

`DaemonHandle` provides `shutdown()` (signal + abort) and `wait()` (block until
natural exit). The IPC listener, vsock listener, and all spawned tasks respect
the shutdown `watch` channel.

### TUN Cleanup
**Where:** `crates/daemon/src/netlink.rs:169-188`

TUN interfaces are deleted via `ip link delete` when peers disconnect.
Best-effort: "Cannot find device" is treated as success (already gone).

---

## Architectural Highlights

### Type Erasure for Binary Size
**Where:** Throughout `crates/core/src/client/`, `crates/core/src/server/`

Both `ConnectResult` and `AcceptResult` have `.erase()` methods that convert
from generic `<T: Transport>` to `Arc<dyn ErasedTransport>`. This is done
synchronously before spawning async tasks, so the async state machine is
monomorphized only once regardless of transport type. Keeps binary size
manageable despite supporting two transports.

### Wait-Free Data Structures
**Where:** `crates/core/src/control/`

All shared state (peers, routes, metrics, node state) uses `ArcSwap` or atomics
for wait-free reads. The hot path (data plane) never blocks on a mutex.

### Separation of Concerns
**Where:** Crate boundaries

- `wire` — Protobuf definitions only, no logic
- `transport` — Transport trait + implementations, no tunnel logic
- `core` — All tunnel logic, transport-agnostic
- `daemon` — OS integration (TUN, netlink, DNS), mode orchestration
- `cli` — Argument parsing, REPL, output formatting
- `api` — REST API, completely optional
- `mcp` — AI agent integration, completely optional
- `entry-stack` — Userspace TCP/IP stack, standalone library
- `exit-adapter` — Exit node session management, standalone library
- `ipc` — IPC client library, usable independently

### Zero `unsafe` in Production
**Where:** Workspace lints

`unsafe_code = "deny"` across the workspace. The ICMP session's `MaybeUninit`
buffer in `exit-adapter` is the only `#[allow(unsafe_code)]` in the codebase,
and it's a standard pattern for `socket2::recv`.

---

## Security / OPSEC Notes

### Default Posture is Unauthenticated
TLS encryption but no peer verification by default. Any node that can reach the
listener can connect. This is intentional for low-friction deployment in labs
and CTFs. Add `--psk` for real engagements.

### Security Posture Auto-Hardening
**Where:** `docs/tasks/13f-security-posture.md`

Providing any authentication flag (`--psk`, `--ca`, `--accept-fingerprint`)
automatically suppresses auto-negotiation and auto-routing. The node won't
change roles unexpectedly and won't leak network topology in handshake routes.
Override with `--zero-config` to explicitly re-enable both.

### Route Announcements Leak Topology
Exit nodes announce their local CIDRs in the handshake. On a real engagement,
use `--no-announce-routes` to suppress this. On the entry side,
`--no-accept-routes` prevents auto-installing routes from untrusted peers.

### Auto-Relay Promotion Opens Ports
Auto mode can promote a node to relay, which opens a listener port. Port
scanners and firewall anomaly detection will see it. Use `--role exit` on
target nodes to prevent this.

### PSK via Environment Variable
`WALLHACK_PSK` env var avoids the key appearing in process command lines or
shell history.

### TUN Visibility
Entry nodes create TUN interfaces and modify the routing table. This is visible
to EDR and `auditd`. Use `--role exit` on target hosts to suppress TUN
creation entirely.

### PSK Failure Rate Limiting
**Where:** `crates/daemon/src/mode/`

Failed PSK attempts are deduplicated per-IP with power-of-two logging (1, 2, 4,
8... failures logged). Prevents log spam from brute-force attempts.

### Version String Contains Build Metadata
Version format: `0.8.2+d342586.20260316T083456.release` — includes git SHA,
timestamp, and build profile. Useful for verifying which binary is deployed
where across a multi-node range.

---

## Dropper (Spec — Not Yet Built)

### Fileless Binary Delivery
**Where:** `docs/specs/DROPPER.md`

A planned minimal bootstrap binary for deployment through constrained channels
(web shells, paste buffers, exploit payloads):

- Same CLI as the full binary — no behavioral difference visible to the target
- Downloads the full wallhack binary from the entry node over QUIC or WebSocket
- **Linux:** executes via `memfd_create` (fileless, no disk write) with memory
  sealing (`F_SEAL_*`). Falls back to writing to `/tmp/.<random_hex>`, unlinking
  before exec
- **Windows:** `CreateProcess` from `%TEMP%`
- Wire protocol: 8-byte binary request (`WHDR` magic + OS + arch), response with
  SHA-256 hash for integrity
- Target binary sizes: TCP variant ~150-200 KB, QUIC variant ~400-500 KB
  (statically linked musl)
- Entry node serves its own binary by default; detects dropper vs full-node
  connections by magic bytes

---

## Zero-Config Philosophy

### Everything Just Works
**Where:** `docs/tasks/13-zero-config-and-friends.md`

The guiding design principle: a new operator should be able to set up a
multi-hop tunnel with just `--connect` and `--listen` flags. No manual role
assignment, no route configuration, no certificate management needed for basic
use.

- TLS: self-signed cert auto-generated at startup
- Role: auto-negotiated from capabilities
- Routes: auto-installed from peer handshake advertisements
- TUN: auto-created with deterministic name from peer identity
- Reconnect: automatic with backoff
- Cleanup: TUN interfaces and routes removed on disconnect

### Indeterminate is a First-Class State
When roles cannot be resolved (e.g. both peers have TUN capability), neither
side disconnects. The transport stays alive, the control plane keeps running
(pings continue), and the connection waits for the topology to change. This is
not an error — it's valuable when firewall state and NAT mappings are expensive
to re-establish.

---

## Additional Transport Details

### WebSocket Server Implementation
**Where:** `crates/core/src/server/ws/mod.rs`, `crates/transport/src/websocket/`

The WebSocket upgrade is custom-implemented (not a library framework), meaning
the server HTTP response is minimal and does not leak framework fingerprints.
The server supports:

- TLS and plain text modes
- Configurable WebSocket path
- mTLS client certificate verification
- Custom yamux configuration (256 KiB receive window per stream)

### WebSocket Byte Stream Adapter
**Where:** `crates/transport/src/websocket/adapter.rs`

Converts between WebSocket message framing and Tokio's `AsyncRead`/`AsyncWrite`
byte stream interface. Uses a read buffer with cursor tracking for partial reads.
Binary messages are used exclusively (no text frames).

### QUIC Transport Details
**Where:** `crates/transport/src/quic.rs`

Thin wrapper around `quinn::Connection` implementing the `Transport` trait.
Exposes the underlying connection for channel binding extraction. Stream limits:
10,000 concurrent bidi streams (client), 1,024 (server).

### Configurable TCP/UDP Buffer Sizes
**Where:** `crates/entry-stack/src/config.rs`

The entry stack's smoltcp TCP sockets use 256 KiB TX + 256 KiB RX buffers by
default, tuned for high throughput. UDP sockets use 256 KiB buffers. All
configurable via `StackConfig`.

---

## CI / Supply Chain

### Binary Size Enforcement
**Where:** `bench/check_bloat.sh`

CI enforces binary size thresholds. Every PR that increases binary size requires
explicit threshold bumps. Current targets: slim build ~5.2 MiB, full build
~7.1 MiB (musl x86_64).

### Dependency Auditing
**Where:** `deny.toml`

Uses `cargo-deny` for license checking and advisory database scanning.

### Cross-Compilation
**Where:** `Cross.toml`, `.github/workflows/`

Primary build target: `x86_64-unknown-linux-musl` for fully static binaries.
Release workflow cross-compiles via `cross`.

### CI Pipeline
**Where:** `.github/workflows/pr.yml`

PR checks run `cargo clippy --all-targets` on both slim and default feature
sets with `-D warnings`. Tests, formatting, and binary bloat checks are all
enforced.

---

## Egress Restriction Layers (Range)

### Realistic Firewall Simulation
**Where:** `range/layers/egress-*/`

The range includes iptables-based egress restriction layers that simulate real
corporate environments:

- **egress-none** — All TCP and UDP outbound blocked. For testing
  reverse-connect scenarios where the exit node must listen.
- **egress-web-only** — Only ports 80 and 443 (TCP) allowed. Forces WebSocket
  transport.
- **egress-ssh-only** — Only port 22 (TCP) allowed.
- **egress-udp53-only** — Only UDP port 53 allowed. A hint at planned DNS
  tunneling transport. Currently used with QUIC listening on :53.

---

## REST API Auth Details

### Constant-Time Password Comparison
**Where:** `crates/api/src/auth.rs`

HTTP Basic Auth credentials are compared using `subtle::ConstantTimeEq` to
prevent timing side-channels. Auto-generated 32-character random secret if
`--api-secret` is not provided.

### Host Header Validation
**Where:** `crates/api/src/validation.rs`

DNS rebinding protection: the REST API validates the Host header against
localhost variants, `[::1]`, and numeric loopback addresses. Rejects requests
with unexpected Host values.

---

## AI Disclosure

### Transparent AI Usage
**Where:** `AI_DISCLOSURE.md`

The project openly discloses its use of AI tools in development. Claude Code
commits are co-authored with explicit attribution.
