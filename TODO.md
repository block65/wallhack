# Wallhack TODO

## Performance

- [ ] Reverse throughput asymmetry: `tcp_entry_client` ~900 Mbps vs `tcp_exit_client`
      ~190 Mbps (QUIC). Increasing recv buffer from 1500→65536 had no effect, so
      it is not per-message protobuf overhead. Likely in `SyscallExitAdapter` —
      investigate how TCP connections are managed in the exit adapter, poll loop
      wakeup latency, and mutex contention between smoltcp writes and the poll loop.
- [ ] Buffer pooling for UDP packets and TUN reads
- [ ] Reduce global lock contention in entry-stack
- [x] ~~Broadcast → mpsc migration on data path~~ — done.

## Transports

- [ ] DNS over tunnel — intercept DNS queries at the TUN interface on the exit
      node and route them back through the tunnel for resolution on the entry
      side, preventing DNS leaks on the target network that would otherwise be
      visible to IDS/monitoring. `hickory-resolver` (`dns-resolver` feature) is
      already present and would handle resolution on the entry side.
- [ ] DNS transport — encapsulate tunnel traffic inside DNS queries/responses
      to egress through firewalls that block all TCP/UDP except port 53.
- [ ] Full ICMP support — forward control messages (e.g., Time Exceeded,
      Parameter Problem) to support diagnostic tools like `traceroute` and
      enable Path MTU Discovery.
- [ ] ICMP as an egress transport — encapsulate the tunnel inside ICMP Echo
      packets to bypass firewalls that block all outbound TCP/UDP.
- [ ] HTTP/2 multiplexing
- [ ] Domain fronting support
- [ ] Deterministic TUN addresses based on peer identity

## REPL

- [ ] `shell` — spawn shell over tunnel
- [ ] Per-peer traffic stats — `stats [<peer>]` showing bytes/packets per peer
      rather than global node aggregates. Requires per-peer counters in
      `Metrics`/`Registry`.
- [ ] Runtime mode promotion — e.g. promoting exit → relay via CLI command.
      Entry → relay is nonsensical. Could be `wallhack promote relay` or similar.
- [ ] Relay direction — how does a relay decide which direction to forward?
      Needs investigation.

## REST API

- [ ] `connect` and `listen` endpoints — expose `NodeApi::connect` and
      `NodeApi::listen` over the REST API so nodes can be managed
      programmatically (e.g. orchestration scripts, web dashboards).
- [ ] Periodic latency measurement via control channel ping/pong

## Security

- [ ] Certificate pinning
- [ ] Encrypted config storage

## Testing

- [ ] Integration test for full pivot chain
- [ ] Fuzzing for protocol parsers
- [ ] Chaos testing (network partitions, latency)
- [x] ~~Benchmark scenario naming~~ — renamed `tcp_fwd` → `tcp_upstream`,
      `tcp_rev` → `tcp_downstream`. Both are measured from the entry node's
      POV (iperf3 client always on entry, server always on exit at `ECHO_PRIV`).
      Upstream = entry sends; downstream = entry receives (server sends via `-R`).

## Bugs

- [ ] TUN EBUSY on rapid reconnect — `create_tun_with_retry` (entry) retries
      3× at 500ms but the previous `TunActor` hasn't been fully dropped before
      the new connection attempts to claim the same TUN name. Rapid
      connect/disconnect cycles accumulate stale connections, eventually causing
      resource exhaustion and process kill (OOM or SIGKILL). Needs proper TUN
      lifecycle tracking — ensure the old actor is fully dropped before allowing
      a new connection to reuse the name.
- [x] ~~No color in `[+]` notification messages~~ — done, uses `nu-ansi-term`
      behind `repl` feature gate.
- [ ] Log prefix inconsistency in REPL — mix of `warn:` prefix (from
      `tracing::warn!`) and `[+]`/`[-]`/`[!]` prefixes (from notifications).
      Consolidate into a consistent style. Broader fix: unified logging format
      — logfmt at most (e.g. `tracing-logfmt`) for `wallhackd`
      background/systemd (non-slim only), consistent prefixed format for
      foreground/REPL (slim always uses this). Watch bloat.
- [ ] Relay mode: `control_tx` dropped immediately — `relay.rs` and `exit.rs`
      relay-capability paths call `.channels().clone()` then drop the
      `ConnectResult`, killing the upstream control stream. Need to retain
      `control_tx` (e.g. via `into_parts()` or holding the `ConnectResult`).
- [ ] Relay mode: no peer registration — pure relay has no `Registry` at all;
      exit relay-capability mode passes peers to `ServerOptions` but
      `run_accept_bridge_loop` / `bridge_channels` never calls `register()` /
      `unregister()`. REPL `peers` always empty in relay mode.
- [ ] Noisy reconnect messages on exit node after entry exits — multiple
      overlapping messages ("Connection tasks died", "Transport disconnected",
      "Connection dropped") fire at non-verbose log levels. Consolidate into a
      single clean message.
- [x] ~~Website CLI docs ping description~~ — updated to "Measure latency to
      a peer".

## UX

- [ ] XDG config file (`~/.config/wallhack/config.toml` or similar) for
      persistent user preferences — e.g. `require_auth = true` to opt into
      enforced PSK/mTLS for users who always want auth and want a hard failure
      rather than a warning.
- [ ] Update the website with the benchmarks, explain that they are just "in
      the gigabits per second" and its kind of irrelevant because the tunnel
      isn't a bottleneck, its the OS or the VM. Confirm this makes sense first.
      Some benchmarks are below 1 Gbps, which should be quoted. Latency can be
      quoted also. Maybe we can just say "1 Gbps+". Its like a weird flex
      because we can't say.

## Build / Config

- [ ] Release process: `feat:` commits trigger a minor (0.x.0) bump, which
      per semver pre-1.0 signals breaking changes. Make `feat:` a patch bump
      while pre-1.0, reserve minor bumps for genuinely breaking changes.
- [ ] Drop glibc release builds in favour of musl static only. Add
      `aarch64-unknown-linux-musl`. Consider armv7 for older 32-bit targets.

## Code Quality & Architecture

- [ ] Newtype wrappers for domain primitives — `PeerId`, `PeerName`, `Psk` are
      raw `String`; port numbers are raw `u16`. Introduce newtypes
      (`struct Psk(String)` with `ZeroizeOnDrop`, `struct PeerId(String)`) so
      the compiler prevents mixing, reduces ambiguous clone noise, and documents
      intent at the type level.
- [x] ~~`Metrics` field visibility~~ — fields now private with `snapshot()`
      accessor returning `node_api::Metrics`.
- [ ] Redundant role conversion helper — `crates/core/src/negotiate.rs`
      has `proto_to_core_role()` even though `impl From<ProtoNodeRole> for
      NodeRole` already exists in `crates/core/src/types.rs`. Replace the free
      helper with `.into()` and remove the duplicate conversion logic.
- [ ] `run_control_loop` parameter object — seven parameters (four of which are
      `Option<&Tx>`) in `crates/core/src/transport/bridge.rs`. Group the channel
      handles into a `ControlLoopHandles` struct to enforce all-or-nothing
      wiring, improve call-site readability, and remove the `Option`-juggling
      inside the loop. **See docs/tasks/10-type-system-improvements.md (item 4).**
- [ ] `ControlLoopExit::Disconnect` reason — `Disconnect(String)` carries a raw
      reason string. Replace with a `DisconnectReason` enum so exhaustive match
      catches new variants and callers can react structurally rather than parsing
      strings. **See docs/tasks/10-type-system-improvements.md.**
- [ ] `ConnectionManager` decomposition — `crates/core/src/entry/manager.rs`
      handles TCP accept, UDP forwarding, SYN proxy dispatch, UDP session GC,
      and exit response handling in one `select!` loop. Exit response handling
      was extracted to `handle_exit_response()` but the main loop is still
      monolithic. Extract each remaining concern into a focused type or async fn
      with a clean input/output contract.
- [ ] `run_auto_accept_session` decomposition — `crates/daemon/src/mode/auto.rs`
      handles entry and exit negotiation outcomes in one function. Extract the
      entry and exit arms into dedicated helpers with clean signatures, but only
      after the transport-monomorphization fix lands (otherwise decomposition
      without deduplication adds binary size).
- [ ] Auto-mode session parameter object — `run_auto_connect_session_dispatch()`
      and `run_auto_accept_session_inner()` in `crates/daemon/src/mode/auto.rs`
      thread large groups of values peeled out of `ConnectResult` /
      `AcceptResult`. Introduce a small erased session context struct instead of
      re-passing transport, channels, control, metrics, peers, and peer address
      as separate arguments.
- [ ] Unify auto-connector outgoing stream setup — `run_auto_connect_session`
      manually spawns the send-instructions or send-responses task after
      negotiation because the client connected with `NodeRole::Indeterminate`
      (no outgoing task). Consider a first-class post-negotiation "promote role"
      API on `ConnectResult` so auto mode does not need to reach into transport
      internals directly.
- [ ] `AcceptResult` / `ConnectResult` construction cleanup —
      `crates/core/src/server/server.rs` and `crates/core/src/client/client.rs`
      still use wide constructors (`AcceptResult::with_handshake`,
      `ConnectResult::new`). Replace them with a builder or narrower staged
      constructors so connection assembly is less argument-heavy and more
      idiomatic.
- [ ] `start_api()` config bundling — `crates/daemon/src/mode/entry.rs`
      passes `metrics`, `peers`, `routes`, TLS config, username, and secret as
      separate arguments even though `EntryResources` already groups part of the
      state. Take a dedicated API config/resources object instead of threading
      sibling fields individually.
- [ ] Naming convention violations — `crates/core/src/node_api.rs` uses
      `upstream`/`downstream` in doc comments and method descriptions (the
      public control API trait). Should use `peer`/`relay` terminology per
      AGENTS.md naming conventions. Transport-layer usage (driver.rs, etc.) is
      fine.

## Sockets / IPC

- [ ] Consider `dirs` or similar for working out where the socket goes. Needs a
      very careful judgement on BLOAT! (Currently custom XDG/fallback logic in
      `crates/core/src/ipc.rs`.)
- [ ] No in-memory IPC for multi-binary mode — socket always goes to disk. When
      running in multi-binary mode, the IPC channel should be in-memory.
- [ ] Socket permissions not explicitly set — `UnixListener::bind()` relies on
      process umask. Should explicitly set permissions to prevent hijacking in
      shared environments, while remaining compatible with systemd.
- [ ] Windows IPC not implemented — Unix socket only. A `TODO` comment in
      `crates/core/src/ipc.rs` notes the need for platform-agnostic named pipes.
      macOS works (Unix sockets).

## Dropper

- [ ] Self-extracting payload format
- [ ] In-memory execution (no disk write)
- [ ] Polymorphic stub generation
- [ ] Anti-sandbox checks
- [ ] Hard mode cyber range (dropper deployment demo)

## Security Advisories

- [ ] AWS-LC: Timing side-channel in AES-CCM tag verification (high) — `aws-lc-fips-sys`, `aws-lc-sys`
- [ ] AWS-LC: `PKCS7_verify` certificate chain validation bypass (high) — `aws-lc-sys` GHSA-vw5v-4f2q-w9xf


## Website
- [ ] `website.just` file is in the wrong place?