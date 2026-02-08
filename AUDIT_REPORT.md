# Wallhack Architecture & Implementation Audit (v2)

**Date:** 2026-02-07
**Scope:** Full codebase (~10,000 LOC Rust across 6 crates)
**Perspective:** Senior Rust developer, pentesting tool focus

---

## Executive Summary

Wallhack is a well-architected Layer 3 tunneling tool with solid fundamentals: proper workspace structure, consistent crypto stack (rustls/ring), clean trait abstractions, and a disciplined approach to `unsafe` (forbidden workspace-wide). Both QUIC and WebSocket transports are fully implemented and benchmarked — QUIC hits 1,400 Mbps single-stream (2.9 Gbps multi-stream), WebSocket hits 1,126 Mbps single-stream (4.5 Gbps multi-stream). Reverse connections (exit connects to entry) work correctly.

The primary gaps are in security hardening (TLS verification disabled by default, no tunnel authentication) and operational polish (session limits, config persistence, remote network discovery).

This report corrects several inaccuracies from the v1 audit, notably: WebSocket transport is complete (not stubbed), reverse connections work (the `todo!()` calls are for a different feature — exit-side port binding), and SOCKS/port-forwarding are intentionally out of scope for this tool.

---

## Table of Contents

1. [Corrections From v1 Audit](#1-corrections-from-v1-audit)
2. [Comparison: Wallhack vs Ligolo-ng](#2-comparison-wallhack-vs-ligolo-ng)
3. [Quick Wins](#3-quick-wins)
4. [Critical Security Issues](#4-critical-security-issues)
5. [Architecture & Design](#5-architecture--design)
6. [Performance & Data Path](#6-performance--data-path)
7. [Code Quality & Idioms](#7-code-quality--idioms)
8. [Memory Management](#8-memory-management)
9. [Error Handling](#9-error-handling)
10. [Testing & Benchmarks](#10-testing--benchmarks)
11. [Build & CI](#11-build--ci)
12. [Prioritised Action Plan](#12-prioritised-action-plan)

---

## 1. Corrections From v1 Audit

### 1.1 WebSocket Transport Is Fully Implemented (v1 was WRONG)
The v1 report implied WebSocket might be incomplete. It is not. The WebSocket transport (`crates/transport/src/ws.rs`, 483 lines) is production-ready with:
- Full server listener (`crates/wallhack/src/server/ws/mod.rs`, 248 lines)
- Full client connector (`crates/wallhack/src/client/ws/mod.rs`, 367 lines)
- Yamux-based stream multiplexing with uni + bi streams
- TLS/mTLS support
- Unit tests for concurrent streams, bidirectional communication, and connection closure
- **Benchmarked**: 1,126 Mbps single stream, 4.5 Gbps with 4 parallel streams

### 1.2 Reverse Connections Work (v1 conflated two different features)
The v1 report listed "Reverse Port Forwarding" as unimplemented. This conflated two things:

1. **Reverse transport connection** (exit connects TO entry) — **WORKS**. The `--connect` flag on exit nodes and `--listen` on entry nodes establishes the tunnel in reverse direction. This is the normal operating mode for pentesting (agent on target calls back to operator).

2. **Exit-side port binding** (`TcpListenInstruction`) — **UNIMPLEMENTED** (`todo!()`). This is a different feature: binding a port on the exit node's network and forwarding incoming connections back through the tunnel. The `todo!()` calls at `exit/net/tcp.rs:155,162` are for this.

**Clarification**: Wallhack is a transparent L3 tunnel. It sets up the communication channel between peers and the rest is up to the pentester. If you want reverse shells, you run them over the tunnel. This is the correct design — it's not a port forwarding tool.

### 1.3 SOCKS Proxy and Port Forwarding Are Out of Scope (v1 was WRONG)
The v1 report listed SOCKS5 and SSH-style port forwarding as "missing features." These are **intentionally not in scope**. Wallhack creates a transparent Layer 3 tunnel via TUN interfaces. Traffic enters the TUN and appears on the exit node's network. If you want SOCKS, run a SOCKS proxy over the tunnel. If you want SSH tunnels, SSH through the tunnel. This is the same model as ligolo-ng and is the correct approach for this class of tool.

### 1.4 Comprehensive Test Suite Exists (v1 understated this)
The `bench/` directory contains a full pytest + iperf3 + network namespace test infrastructure:
- 10 test files covering smoketest, TCP benchmarks, UDP, parallel streams, WebSocket, reverse mode, infrastructure
- 4-namespace topology (client, entry, exit, target) with veth pairs
- iperf3 throughput benchmarks with results logging
- `range/` directory with Docker Compose cyber range (10 custom images)
- Justfile automation for build, test, benchmark, and cleanup

### 1.5 Benchmark Results (actual measured performance)

From `bench/results/benchmark_latest.txt` (post Stage 4 optimizations):

| Transport | Streams | Throughput |
|-----------|---------|------------|
| QUIC | 1 | 1,400 Mbps |
| QUIC | 2 | 2,946 Mbps |
| QUIC | 3 | 2,921 Mbps |
| QUIC | 4 | 2,863 Mbps |
| QUIC | 5 | 2,739 Mbps |
| WebSocket | 1 | 1,126 Mbps |
| WebSocket | 1 (fixture) | 1,135 Mbps |
| WebSocket | 2 | 3,696 Mbps |
| WebSocket | 3 | 4,496 Mbps |
| WebSocket | 4 | 4,320 Mbps |

All 20 cargo tests pass. All 15 netstack tests pass. WebSocket multi-stream actually outperforms QUIC multi-stream.

---

## 2. Comparison: Wallhack vs Ligolo-ng

Both tools solve the same problem the same way: TUN interface on the operator's machine, userspace network stack translates IP packets to socket operations, agent/exit makes syscalls on the target network. Neither requires root on the target.

### 2.1 Architecture Comparison

| Aspect | Ligolo-ng | Wallhack |
|--------|-----------|----------|
| Language | Go | Rust |
| Components | Proxy + Agent | Entry + Exit (+ Relay) |
| Userland netstack | Google gvisor | smoltcp |
| Wire protocol | Custom binary | Protobuf (versioned) |
| Primary transport | TCP/TLS | QUIC (UDP) |
| Fallback transport | None | WebSocket (TCP) |
| Memory safety | Go GC | `unsafe_code = "forbid"` |
| Head-of-line blocking | Yes (TCP) | No (QUIC) |
| Stream multiplexing | Custom (Go channels) | QUIC native / yamux |

### 2.2 What Ligolo-ng Has That Wallhack Lacks

| Feature | Impact | Effort to Add |
|---------|--------|---------------|
| **Certificate fingerprint verification (TOFU)** | Default security against MITM | 2-3 days |
| **Let's Encrypt auto-cert (`-autocert`)** | Legitimate-looking TLS traffic | 2-3 days |
| **`ifconfig` command** (remote network discovery) | Essential for pivot planning | 1-2 days |
| **Configuration persistence** (routes/tunnels survive restart) | Critical for long engagements | 2-3 days |
| **Web UI** (v0.8+) | Multi-agent management | 1-2 weeks |
| **Daemon mode** | Headless operation as service | 1-2 days |
| **Cross-platform pre-built binaries** | Deploy without compiling | CI config (1 day) |

**Note:** Multi-agent session switching is NOT a gap — wallhack already supports multiple concurrent exit nodes with per-peer TUN interfaces and per-peer route management. Ligolo-ng's `session` command selects an *active* agent for interaction; wallhack's model is closer to "all agents active simultaneously." Similarly, agent SOCKS proxy egress is not a gap — wallhack's WebSocket transport over TCP naturally flows through corporate HTTP proxies.

### 2.3 What Wallhack Has That Ligolo-ng Lacks

| Feature | Advantage |
|---------|-----------|
| **QUIC transport (UDP)** | Different firewall profile, no head-of-line blocking, better on lossy links. This is wallhack's killer feature — no other pentest tunneling tool uses QUIC. |
| **Dual transport** (QUIC + WebSocket) | Fallback when UDP is blocked, protocol selection via `host:port/tcp` |
| **Protobuf wire protocol** | Formal schema, versioned, extensible, language-agnostic |
| **mTLS support** (implemented) | Mutual authentication — ligolo-ng lists this as "Todo" |
| **Separate control plane** | Stats, peers, ping, routes over dedicated streams |
| **Relay as first-class concept** | Dynamic relay capability via REPL commands |
| **Per-connection latency measurement** | In-protocol ping/pong surfaced in REPL |
| **REST API** | Programmatic control for C2 integration |
| **Dynamic exit mode transitions** | Connect/disconnect/listen at runtime without restart |

### 2.4 Lessons From Ligolo-ng's Success

1. **Extreme setup simplicity** — ligolo-ng's quickstart is ~4 commands. Wallhack should aim for the same: one command to start entry, one to start exit, traffic flows.
2. **Pre-built multi-platform binaries** — pentesters don't want to compile. Ship binaries for linux-amd64, linux-arm64, windows-amd64, macos-amd64/arm64.
3. **TOFU as default** — print fingerprint on entry startup, verify on exit connect. This is the minimum viable security model and is immediately usable.
4. **Remote network visibility** — `ifconfig` equivalent is essential for pivot planning. Without it, the operator must manually enumerate from the tunnel.
5. **"Just works" defaults** — auto-generated certs + fingerprint verification = secure and zero-config.

---

## 3. Quick Wins

Things that can be done in under a day each and provide immediate value:

### 3.1 Replace `todo!()` with Proper Errors (1 hour)
**Files:** `exit/net/tcp.rs:155,162`, `orchestrator.rs:596`

Three `todo!()` macros will panic at runtime. Replace with `Err(RuntimeError::new("tcp_listen not yet implemented"))`. Even if the feature isn't built yet, don't crash the process.

### 3.2 Replace Hand-Rolled Base64 with `base64` Crate (30 min)
**File:** `api/auth.rs:106-136`

The `base64` crate is already a transitive dependency. Replace the 30-line hand-rolled decoder with `base64::engine::general_purpose::STANDARD.decode()`.

### 3.3 Fix Timing-Unsafe Credential Comparison (30 min)
**File:** `api/auth.rs:47`

Replace `u == username && p == password` with `ring::constant_time::verify_slices_are_equal()`. Ring is already a dependency.

### 3.4 Make ALPN Configurable or Update (30 min)
**File:** `server/tls.rs:7`

`"hq-29"` is an old HTTP/3 draft identifier. Either update to a current one or make it configurable via CLI flag for evasion flexibility.

### 3.5 Centralise Magic Numbers (half day)
**Files:** multiple

Create a `config` module with named constants:
- `orchestrator.rs:139` — `let mtu = 1500;` hardcoded in closure
- `bridge.rs:25` — `TUNNEL_MTU: usize = 2000`
- `bridge.rs:28` — `CONTROL_MTU: usize = 4096`
- `ws.rs:32` — `PREFIX_READ_TIMEOUT: Duration::from_secs(5)`
- Various `32` channel capacity values in `ws.rs`

### 3.6 Add `panic = "abort"` and `codegen-units = 1` to Release Profile (10 min)
**File:** `Cargo.toml`

Reduces binary size and improves LTO effectiveness. Already has `strip = true`, `opt-level = 3`, `lto = true`.

### 3.7 Add `cargo-deny` (1 hour)
Add `deny.toml` for dependency auditing. Catches known-vulnerable dependencies, license issues, and duplicate crate versions. Essential for a security tool.

### 3.8 Box Large Enum Variants (30 min)
**Files:** `client/ws/mod.rs:53`, `server/ws/mod.rs:77`

Clippy already flags these: `MaybeTlsStream::Tls` variant is 1096/1208 bytes while `Plain` is 40 bytes. Box the TLS variant to reduce stack usage.

### 3.9 Clean Up Dead Code (1 hour)
**Files:** multiple

- `orchestrator.rs:52-54` — commented-out method
- `client/tls_config.rs:37-42` — underscore-prefixed unused function
- `protobuf/src/socket_set.rs:6-13` — commented-out error enum
- Commented-out proto fields

Git preserves history. Delete it.

### 3.10 Add `ifconfig` / `interfaces` REPL Command (1-2 days)
Query the exit node's network interfaces and display them. Essential for pivot planning. Can be implemented as a new `ControlRequest` variant that calls `getifaddrs()` on the exit side and returns interface info. This is the single highest-value feature gap vs ligolo-ng.

---

## 4. Critical Security Issues

### 4.1 TLS Certificate Verification Disabled By Default
**Severity: CRITICAL | Files: `tls/verifiers.rs`, `client/tls_config.rs:30-35`**

`SkipServerVerification` accepts ANY certificate. The client config uses this as the default when no mTLS is configured:
```rust
None => with_great_danger(), // Skips ALL cert verification
```

For a tunneling tool used in adversarial networks, this is the single most dangerous issue.

**Fix:** Implement certificate fingerprint verification (TOFU model, like ligolo-ng). Print fingerprint on entry node startup, require exit node to verify it via `--accept-fingerprint`. This is the minimum viable security model.
**Effort:** 2-3 days

### 4.2 No Tunnel Authentication
**Severity: CRITICAL | Files: `server/`, `transport/bridge.rs`**

No authentication between entry and exit nodes. Anyone who can reach the listening port can connect and use the tunnel. The `ExitNodeHello` message contains `exit_id` and `version` but no authentication token.

**Fix:** PSK-based authentication: shared secret via CLI, used for initial handshake. More robust: wire mTLS as default (infrastructure exists but isn't the default path).
**Effort:** 2-3 days for PSK, 1 week for full mTLS flow

### 4.3 No Session or Connection Limits
**Severity: HIGH | Files: `exit-adapter/`, `exit/orchestrator.rs`**

The exit node has no maximum session count, no per-session timeout, no idle connection timeout, no rate limiting. The orchestrator spawns unbounded `tokio::spawn()` for every instruction. UDP sessions in `DashMap` are never cleaned up.

**Fix:** Add `tokio::sync::Semaphore` for concurrent task limiting. Add session TTL with reaper task. Add `DashMap` capacity bounds.
**Effort:** 2-3 days

### 4.4 Plaintext Credential Comparison in API Auth
**Severity: MEDIUM | File: `api/auth.rs:45-49`**

Direct string comparison is timing-attack vulnerable. Custom base64 decoder instead of existing crate. See Quick Wins 3.2 and 3.3.

---

## 5. Architecture & Design

### 5.1 Clean Crate Separation (Positive)
Well-organized workspace with focused responsibilities. The trait-based abstractions (`Transport`, `ExitAdapter`, `Server`) enable testing with mocks and swapping implementations.

### 5.2 State Machine Design (Positive)
The exit node REPL uses a clean pattern: `run()` → outer loop dispatching to `run_idle_mode()`, `run_connect_mode()`, `run_listen_mode()`, `run_relay_capability_mode()`, each returning `ExitAction` for transitions. REPL is created once and persists across mode transitions.

### 5.3 NodeApi Architecture: REPL vs REST API
**Assessment: Acceptable but with a validation gap**

The architecture has two clients of the same shared state:

```
┌──────────────────────────────────────────┐
│           Shared State Layer             │
│  ┌──────────┬──────────┬──────────┐     │
│  │ Metrics  │ Registry │ Routes   │     │
│  └──────────┴──────────┴──────────┘     │
└──────────────────────────────────────────┘
       ↑                        ↑
       │                        │
┌──────┴──────┐    ┌────────────┴────────┐
│  REST API   │    │  REPL               │
│ (via NodeApi│    │ (direct access)     │
│  + Handler) │    │                     │
└─────────────┘    └─────────────────────┘
```

The REST API goes through `NodeApi` trait → `Handler` implementation, which validates inputs and enforces role-based access. The REPL directly accesses `SharedMetrics`, `SharedRegistry`, and `SharedRouteTable`.

**This is fine architecturally** — the REPL is a local, trusted, interactive interface. It doesn't need HTTP middleware, auth, or DNS rebinding protection. However, there's a **validation gap**: the REST API validates peer IDs and CIDR formats via `validation.rs`, while the REPL does minimal validation. If the REPL added routes with invalid CIDRs or peer IDs, they'd be accepted.

**Recommendation:** Share the validation functions. Either have the REPL call `NodeApi` methods, or extract the validation into a shared module that both paths use. This is a quality improvement, not a critical fix.

### 5.4 Dual Tunnel Protocol (Intentional Design)
The codebase has two complementary data paths — investigation confirms this is **intentional design**, present since early commits, not a refactoring artifact:

1. **Bi-stream sessions** (entry/session.rs, exit.rs `handle_stream`) — Raw TCP data tunneling. Entry opens a bi-stream per TCP connection, sends a `SessionInit` message, then uses `copy_bidirectional()` for zero-serialization streaming. This is the **primary data path** for TCP and is efficient: no protobuf encode/decode per packet, stream lifetime = connection lifetime.

2. **Uni-stream instructions/responses** (bridge.rs, orchestrator.rs) — Control plane and non-streaming protocols. Used for: connection lifecycle events (connect, close, listen), UDP single-packet exchanges, ICMP, error reporting, and status updates. Each instruction/response is a protobuf message on a short-lived uni-stream.

| Aspect | Bi-stream Sessions | Uni-stream Messages |
|--------|-------------------|---------------------|
| Purpose | TCP data streaming | Control + UDP/ICMP |
| Serialization | Raw bytes after SessionInit | Protobuf per message |
| Lifetime | Connection duration | Request-response |
| Efficiency | Zero-copy via `copy_bidirectional` | Suitable for discrete events |

This separation is sound: it keeps control plane independent from data plane, avoids serialization overhead for bulk TCP, and allows different handling for streaming vs request-response protocols.

### 5.5 Network Flattening: Works, But Not Automatic

**Network flattening already works.** Two mechanisms:

1. **Direct connections:** Each exit node gets its own TUN (`tun-{exit_id}`). The operator adds OS routes (`ip route add 10.0.1.0/24 dev tun-exit-dmz`). Remote networks appear as directly routable interfaces on the operator's machine.

2. **Relayed connections:** The relay is transport-transparent — bi-streams pass through, broadcast channels are re-broadcast without filtering. All downstream exit networks are reachable through the relay's single TUN. The network is flat.

**What's NOT implemented is automatic network flattening** — two separate jobs:

| Job | Status | Requires |
|-----|--------|----------|
| Network flattening (packets flow) | **Works** | TUN per peer + OS routing, or relay transparency |
| Auto-discovery (entry learns remote networks) | **Not implemented** | Exit nodes report interfaces/routes → entry auto-configures |

The auto-discovery path needs:
- Exit nodes sending interface/route metadata to the entry node (the peer metadata infrastructure exists — currently only latency data propagates via ping/pong)
- Entry node auto-adding OS routes based on received metadata
- For relays: propagating `ExitNodeHello` through the relay (currently consumed by the relay's oneshot channel and not forwarded to the entry node — the entry only sees the relay's identity, not downstream exit identities)

**The `SharedRouteTable`** stores CIDR-to-peer mappings and has REPL/API commands (`route add/del/list`). It's not used for packet forwarding (OS routing handles that), but it's the right place to store auto-discovered routes and could drive automatic OS route configuration.

**The `ifconfig`/`interfaces` command** (Quick Win 3.10) is the first step toward auto-discovery — once exit nodes can report their interfaces, the entry node has the information needed to auto-configure routes.

**Relay identity propagation** is a separate concern: currently `ExitNodeHello` stops at the relay. To support multi-exit visibility through a relay, the hello message would need to be forwarded or the relay would need to aggregate downstream identities. This is a protocol-level change.

#### Planned: Rich Peer Metadata

The exit node should send rich metadata alongside routes — not just latency. This turns the entry node's `peers` and `status` views into a proper situational awareness dashboard for the pentester. The metadata should be sent periodically (not just on connect) so the operator has live visibility.

**Proposed metadata payload** (extend `ExitNodeHello` or add a new `PeerMetadata` message):

| Category | Fields | Pentester Value |
|----------|--------|-----------------|
| **Identity** | exit_id, version, uptime | Know which agent is which, detect restarts |
| **Host OS** | os_type, os_version, kernel, arch, hostname | Target profiling, exploit selection |
| **Resources** | cpu_count, memory_total, memory_available, disk_free | Detect constrained hosts, plan tool deployment |
| **Network** | interfaces (name, ip, netmask, mac, up/down), default_gateway, dns_servers | Pivot planning, network mapping, identify dual-homed hosts |
| **Routes** | local routing table (destination, gateway, interface) | Discover reachable networks without manual enumeration |
| **Latency** | rtt_ms (already implemented via ping/pong) | Connection quality |
| **Process** | pid, running_as_user, privileges (root/admin/unprivileged) | Know what the agent can do, whether to escalate |
| **Environment** | domain/workgroup, is_domain_joined, proxy_settings | AD context, corporate network detection |

**Additional fields worth considering:**
- **ARP table** — discover live hosts on the exit node's local segments without scanning
- **Listening ports** — identify services on the exit node itself (useful if it's a pivot target)
- **Established connections** — see what the exit node is talking to (network context)
- **Firewall rules** (if readable) — understand egress restrictions before tunneling
- **Container detection** — is the exit node in a container/VM? (check for `/.dockerenv`, `/proc/1/cgroup`)

**Implementation notes:**
- Use a new protobuf message (`PeerMetadata`) rather than overloading `ExitNodeHello` — hello is for initial handshake, metadata should be periodic
- Send on connect, then periodically (e.g., every 60s) to catch resource changes
- Entry node stores in the peer registry alongside latency
- Surface via `peers --detail` REPL command and REST API `/peers/{id}/metadata`
- For relays: aggregate downstream metadata and forward to entry (requires relay identity propagation)
- **Security consideration:** some metadata (ARP, connections, firewall rules) is sensitive. Consider making categories opt-in via exit node CLI flags so the operator controls what's exposed

**Effort:** `ifconfig` command: 1-2 days. Auto-route configuration: 2-3 days. Relay identity propagation: 3-5 days. Rich metadata: 3-5 days.

### 5.6 Orchestrator Needs Restructuring
**File: `exit/orchestrator.rs` (600 lines)**

The `Orchestrator::drive()` method is a single ~500-line match arm. Issues:
- No backpressure on task spawning (unbounded `tokio::spawn()`)
- No task tracking (fire-and-forget)
- Deep nesting (7+ levels of indentation in TCP connect handler)

**Fix:** Extract each instruction handler into a separate method. Use `JoinSet` to track spawned tasks. Add semaphore-based concurrency limiting.
**Effort:** 2 days

---

## 6. Performance & Data Path

### 6.1 Excessive Allocations in Hot Path
**Severity: HIGH | Multiple files**

Per-packet allocations in the data path:

1. `orchestrator.rs:180` — `recv_buf[..size].to_vec()` per TCP recv
2. `orchestrator.rs:393` — same for UDP
3. `bridge.rs:56-57` — `Vec::with_capacity(TUNNEL_MTU)` per incoming message, then `Bytes::from(buf)` (another alloc)
4. `ws_adapter.rs:155` — `Message::Binary(buf.to_vec().into())` per WebSocket write
5. `bridge.rs:348` — `Vec::new()` in `write_length_delimited` per message

The outgoing functions (`run_outgoing_instructions`, `run_outgoing_responses`) already reuse buffers — extend this pattern everywhere. Consider `bytes::BytesMut` buffer pools.

**Effort:** 2-3 days

### 6.2 Broadcast Channel for Data Path
**Severity: HIGH | Files: `bridge.rs`, `orchestrator.rs`**

Both instructions and responses use `tokio::sync::broadcast` channels. For a 1:1 tunnel (the common case), every message is cloned unnecessarily. `mpsc` would be more efficient for 1:1; broadcast is only needed for relay/multi-peer.

**Fix:** Use `mpsc` for 1:1 connections. Consider `Arc<Message>` to avoid deep cloning in broadcast scenarios.
**Effort:** 2-3 days

### 6.3 ~~New Uni Stream Per Message~~ (FIXED)
**Severity: MEDIUM | File: `bridge.rs`**

~~Every tunnel message opens a new unidirectional QUIC stream.~~ **Fixed:** Data messages now use persistent streams with length-delimited framing. Single-stream throughput improved from 584→1,400 Mbps (QUIC) and 359→1,126 Mbps (WebSocket).

### 6.4 ~~Netstack 10ms Busy Poll~~ (FIXED)
**Severity: MEDIUM | File: `netstack/src/async_stack/mod.rs`**

~~The poll loop wakes every 10ms regardless of traffic.~~ **Fixed:** Reduced to 1ms with `Notify`-based waking for immediate response to new data.

**Fix:** Implement `AsyncDevice` trait so the poll loop can `await` on TUN read readiness.
**Effort:** 2-3 days

### 6.5 TUNNEL_MTU Hardcoded to 2000 Bytes
**Severity: MEDIUM | File: `bridge.rs:25`**

With protobuf overhead + 1500-byte IP packets, this is tight. TCP data responses with 1500-byte payloads could approach this limit.

**Fix:** Increase to 4096 or make configurable.
**Effort:** 1 hour (see Quick Wins)

---

## 7. Code Quality & Idioms

### 7.1 Strengths
- `unsafe` forbidden workspace-wide (documented exceptions for `exit-adapter`)
- No `unwrap()` or `panic!()` in production code paths (except the `todo!()` items)
- `clippy::pedantic` enabled
- Consistent error types with `thiserror`
- Good use of `#[must_use]`, `#[non_exhaustive]`
- Proper `tracing` instrumentation throughout
- Feature flags are clean: `quic` (default) / `websocket` / `api` / `full`

### 7.2 Clippy Warnings to Address
From the benchmark run, remaining clippy warnings:
- `transport/src/ws.rs:241` — `single_match` (use `if let`)
- `transport/src/ws.rs:242` — `ignored_unit_patterns` (use `()` instead of `_`)
- `transport/src/ws.rs:83,114,287` — `doc_markdown` (backtick code references)
- `transport/src/ws_adapter.rs:113,117` — `needless_continue`
- `client/ws/mod.rs:53`, `server/ws/mod.rs:77` — `large_enum_variant` (Box the TLS variant)
- `client/ws/mod.rs:137` — `result_large_err`
- `client/ws/mod.rs:165` — `missing_panics_doc`
- `repl/src/relay.rs:150` — `needless_borrow`
- `netstack/tests/` — `unreadable_literal`, `uninlined_format_args`, `manual_assert`

**Effort:** 1-2 hours for all

---

## 8. Memory Management

### 8.1 DashMap Without Capacity Bounds
**File: `exit-adapter/src/adapter.rs:19-22`**

Sessions in `DashMap` are added but UDP/ICMP sessions are never cleaned up. Under sustained traffic, this map grows indefinitely.

**Fix:** Add capacity bounds, session TTL, periodic reaper task.
**Effort:** 1-2 days

### 8.2 TCP Buffer Allocation
Each netstack TCP socket allocates 128KB of buffers (64KB rx + 64KB tx). For 1000 concurrent connections, that's 128MB. Consider smaller defaults (16KB each) with configurable sizing.

**Effort:** Trivial

---

## 9. Error Handling

### 9.1 Strengths
- Consistent `thiserror` derivation
- `#[non_exhaustive]` on public error types
- Graceful degradation (connection failures → response enums, not panics)

### 9.2 String-Based Error Context
Errors frequently converted to strings, losing type information:
```rust
.map_err(|e| TransportError::stream(e.to_string()))
```

**Fix:** Use `#[from]` derives or `Box<dyn Error>` to preserve error chains.
**Effort:** 1-2 days

### 9.3 `todo!()` in Reachable Paths
Three `todo!()` macros at `orchestrator.rs:596`, `exit/net/tcp.rs:155,162` will panic at runtime. See Quick Wins 3.1.

---

## 10. Testing & Benchmarks

### 10.1 Comprehensive External Test Suite (Positive)
The `bench/` directory has excellent infrastructure:
- **10 pytest test files**: smoketest, TCP benchmarks (progressive payloads), parallel streams, UDP, WebSocket, reverse mode, infrastructure validation
- **4-namespace topology**: `wh-client`, `wh-entry`, `wh-exit`, `wh-target` with veth pairs and IP routing
- **iperf3 throughput benchmarks** with JSON output parsing and results logging
- **Justfile automation**: `just build`, `just smoketest`, `just benchmark`, `just clean`
- **Cyber range** (`range/`): Docker Compose with 10 custom images for realistic pentest scenarios
- **Python libraries**: process management, network namespace utilities, echo server

### 10.2 Cargo Test Coverage
- `netstack` — 15 tests (pcap replay, socket accumulation, inner stack)
- `wallhack` — 5 tests (control handler, control server/client)
- `transport` — unit test for WsTransport concurrent streams
- `exit-adapter` — 0 tests (mock adapter is mostly `todo!()`)
- `repl` — 0 tests

**Missing:**
- No unit tests for bridge.rs message routing
- No unit tests for orchestrator instruction dispatch
- No property-based testing for protobuf round-tripping
- No fuzzing targets for network-facing parsers

### 10.3 Fuzzing Recommendation
For a network tool processing untrusted input, add `cargo-fuzz` targets for: protobuf deserialization, `read_length_delimited`, WebSocket upgrade handler, IP packet parser.
**Effort:** 1-2 days

---

## 11. Build & CI

### 11.1 Cross-Compilation for Pentest Deployment
CI currently builds for 6 targets. For pentesting deployment, the exit node binary must be available pre-compiled for at least:
- `x86_64-unknown-linux-gnu` (most servers)
- `x86_64-unknown-linux-musl` (static, portable)
- `aarch64-unknown-linux-gnu` (ARM servers, containers)
- `x86_64-pc-windows-msvc` (Windows targets)

The entry node only needs to run on the operator's machine, so fewer targets are fine.

### 11.2 Nightly vs Stable Mismatch
`rust-toolchain.toml` specifies `nightly`, CI uses `dtolnay/rust-action@stable`. Align these.

### 11.3 Release Profile (Positive + suggestions)
Already has `strip = true`, `opt-level = 3`, `lto = true`. Consider adding:
- `panic = "abort"` — smaller binary, no unwinding overhead
- `codegen-units = 1` — maximum LTO benefit

---

## 12. Prioritised Action Plan

### Tier 1: Critical Security (Do First)
| # | Item | Impact | Effort |
|---|------|--------|--------|
| 1 | Certificate fingerprint verification (TOFU) | Prevents MITM (default secure) | 2-3 days |
| 2 | Tunnel authentication (PSK) | Prevents unauthorized tunnel use | 2-3 days |
| 3 | Session limits + timeouts + reaper | Prevents resource exhaustion | 2-3 days |

### Tier 2: Quick Wins (High ROI, Low Effort)
| # | Item | Impact | Effort |
|---|------|--------|--------|
| 4 | Replace `todo!()` with errors | Prevent runtime panics | 1 hour |
| 5 | Replace hand-rolled base64 | Remove bug surface | 30 min |
| 6 | Fix timing-unsafe auth comparison | Prevent timing attacks | 30 min |
| 7 | Box large enum variants | Reduce stack usage | 30 min |
| 8 | `panic = "abort"` + `codegen-units = 1` | Smaller/faster binary | 10 min |
| 9 | `cargo-deny` | Dependency auditing | 1 hour |
| 10 | Clean up dead/commented code | Code hygiene | 1 hour |
| 11 | Fix remaining clippy warnings | Code quality | 1-2 hours |

### Tier 3: Feature Parity with Ligolo-ng
| # | Item | Impact | Effort |
|---|------|--------|--------|
| 12 | `ifconfig` / `interfaces` command | Pivot planning (essential) | 1-2 days |
| 13 | Configuration persistence (see Appendix B) | Long engagement support | 2-3 days |
| 14 | Cross-platform pre-built binaries | Easy deployment | 1 day (CI) |

### Tier 4: Performance
| # | Item | Impact | Effort |
|---|------|--------|--------|
| 16 | Reduce data path allocations | Throughput improvement | 2-3 days |
| 17 | Switch 1:1 data path to mpsc | Eliminate unnecessary cloning | 2-3 days |
| 18 | Multiplex messages over long-lived streams | 10-30% throughput gain | 3-5 days |
| 19 | Fix netstack 10ms busy poll | Reduce idle CPU, latency | 2-3 days |

### Tier 5: Nice to Have
| # | Item | Impact | Effort |
|---|------|--------|--------|
| 20 | Traffic obfuscation / domain fronting | Evasion | 1-2 weeks |
| 21 | Restructure orchestrator | Maintainability | 2 days |
| 22 | Fuzz targets | Security hardening | 1-2 days |
| 23 | REPL uses NodeApi for validation | Consistency | 1 day |
| 24 | Web UI | Multi-agent management | 1-2 weeks |

---

## Appendix: File Reference

### Security-Critical Files
| File | Concern |
|------|---------|
| `wallhack/src/tls/verifiers.rs` | `SkipServerVerification` — accepts any cert |
| `wallhack/src/client/tls_config.rs` | Default client uses dangerous TLS config |
| `wallhack/src/api/auth.rs` | Timing-unsafe comparison, hand-rolled base64 |
| `wallhack/src/transport/bridge.rs` | No authentication on tunnel messages |

### Performance-Critical Files (Hot Path)
| File | Role |
|------|------|
| `wallhack/src/transport/bridge.rs` | Message routing between transport and channels |
| `wallhack/src/exit/orchestrator.rs` | Instruction dispatch and response collection |
| `wallhack/src/entry/session.rs` | TCP session data forwarding (entry side) |
| `transport/src/ws_adapter.rs` | WebSocket byte stream adaptation |
| `netstack/src/async_stack/mod.rs` | Userspace TCP/IP stack poll loop |

### Unimplemented (but not blocking)
| File | Status | Note |
|------|--------|------|
| `exit-adapter/src/sessions/tcp_listen.rs` | Empty | Exit-side port binding, not reverse connections |
| `exit-adapter/src/sessions/udp_listen.rs` | Empty | Same |
| `exit/net/tcp.rs:155,162` | `todo!()` | Will panic if `TcpListenInstruction` is sent |

---

## Appendix B: Configuration Persistence — Implementation Recommendations

### What to Persist

**Entry node state** (operator-side, persisted across restarts):
- Routes: CIDR → peer_id mappings
- Known peers: peer_id → last-known address, TUN name, fingerprint
- API auth credentials
- TLS cert paths
- Listen address and transport preference

**Exit node state** (agent-side, minimal — avoid leaving forensic artifacts):
- Connect-back address
- Exit ID (for stable TUN naming on reconnect)
- TLS cert/key paths if using mTLS

### Recommended Approach

**Use a single TOML file**, not YAML. TOML is idiomatic for Rust tooling, has excellent serde support, and the `toml` crate is mature. Ligolo-ng uses YAML; wallhack should use the Rust ecosystem's native format.

**File locations:**
- Entry: `~/.config/wallhack/config.toml` (XDG) or `./wallhack.toml` (current directory, takes precedence)
- Exit: no default config file (avoid forensic traces). Only used if explicitly passed via `--config`.

### Suggested Config Schema

```toml
# wallhack.toml (entry node)

[entry]
listen = "0.0.0.0:6565/udp"

[entry.api]
enabled = true
bind = "127.0.0.1:8080"
username = "admin"
password = "changeme"

[entry.tls]
cert = "/path/to/cert.pem"
key = "/path/to/key.pem"

# Routes are auto-saved when added via REPL/API
[[routes]]
cidr = "10.0.1.0/24"
peer_id = "exit-dmz"

[[routes]]
cidr = "10.0.2.0/24"
peer_id = "exit-internal"

# Known peers (auto-populated on first connect)
[[peers]]
id = "exit-dmz"
fingerprint = "sha256:abc123..."
tun_name = "tun-exit-dmz"
```

```toml
# wallhack.toml (exit node - only if --config is passed)

[exit]
id = "exit-dmz"
connect = "operator.example.com:6565/udp"

[exit.tls]
cert = "/path/to/client-cert.pem"
key = "/path/to/client-key.pem"
accept_fingerprint = "sha256:abc123..."
```

### Implementation Strategy

**Phase 1: Read-only config (1 day)**
1. Add `toml` and `dirs` (for XDG paths) to `repl` dependencies
2. Define `Config` struct with `#[derive(Deserialize)]` in a new `crates/repl/src/config.rs`
3. Load config at startup in `wallhack.rs` `main()`: check `./wallhack.toml`, then `~/.config/wallhack/config.toml`
4. CLI flags override config file values (CLI takes precedence)
5. Pass loaded config to `run_entry()` / `run_exit()` instead of raw CLI args

**Phase 2: Auto-save on mutation (1-2 days)**
1. Add `#[derive(Serialize)]` to `Config`
2. When `route add` or `route del` is executed in the REPL, serialize current state to the config file
3. When a new peer connects and provides `ExitNodeHello`, save its ID and fingerprint to the `[[peers]]` section
4. Use `atomicwrites` pattern: write to `config.toml.tmp`, then rename (prevents corruption on crash)
5. On exit node: only auto-save if `--config` was explicitly provided

**Phase 3: Config subcommand (half day)**
```
wallhack config show          # Print effective config (merged CLI + file)
wallhack config init          # Generate default config.toml
wallhack config path          # Print config file location
```

### Key Design Decisions

1. **CLI overrides config file** — always. A pentester must be able to override any saved setting without editing the file. This matches standard Unix tooling behavior.

2. **No auto-save on exit node by default** — the exit binary may be running on a compromised host. Leaving config files is a forensic artifact. Only save if the operator explicitly opts in via `--config`.

3. **Routes auto-save on entry node** — when the operator adds a route via REPL, it should survive a restart. This is the primary pain point that config persistence solves.

4. **Peer fingerprints auto-save** — once an exit node's TLS fingerprint is verified, save it. On reconnect, verify it matches. This implements TOFU persistence.

5. **Use serde for both directions** — `Deserialize` for loading, `Serialize` for saving. The same struct does both. This guarantees the saved config is always loadable.

### Crate Dependencies

```toml
# In crates/repl/Cargo.toml
toml = "0.8"
dirs = "6"        # XDG directory resolution
```

Both are lightweight, well-maintained, and have no transitive bloat.

---

## Conclusion

Wallhack's foundations are strong — clean Rust, correct async patterns, dual working transports, and measured performance that's competitive. The QUIC transport is a genuine differentiator in this tool class. Multi-agent support with per-peer TUN interfaces already works; the route table exists for per-CIDR forwarding but isn't yet wired into the packet path.

The critical gaps are security (authentication, TLS verification) and operational polish (session limits, config persistence, remote network discovery). The Tier 1 and Tier 2 items together represent roughly 2 weeks of work. Addressing them would close the gap with ligolo-ng on security and usability while retaining wallhack's advantages in transport flexibility, protocol design, and runtime safety.
