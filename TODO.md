# Wallhack TODO

## Performance

- [ ] Reverse throughput asymmetry: forward ~3500 Mbps vs reverse ~968 Mbps on
      symmetric `copy_bidirectional` bi-stream path. Investigate Quinn flow control
      defaults, poll loop wakeup latency under egress load, and mutex contention
      between smoltcp writes and the poll loop.
- [ ] Buffer pooling for UDP packets and TUN reads
- [ ] Reduce global lock contention in entry-stack
- [ ] Broadcast → mpsc migration on data path — **see docs/tasks/07-broadcast-to-mpsc.md**.
      `tokio::sync::broadcast` still used for instructions/responses channels;
      `RecvError::Lagged` still present in 5 files. Needs mpsc conversion for
      backpressure and to eliminate silent packet loss.

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

## Auto-role Negotiation

- [ ] Add `ServerHello` protobuf message so the accepting node announces its
      role to the connecting node immediately after the client Hello.
- [ ] Implement auto-role detection: `wallhack --connect <host>` (no subcommand)
      connects, receives the `ServerHello`, and adopts the complementary role
      automatically — entry if the server is exit/relay, exit if the server is
      entry. Peer name defaults to random (same as `exit` today).
- [ ] Drop the subcommand requirement when `--connect` is the only flag.

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

## Bugs

- [ ] TUN EBUSY on rapid reconnect — `create_tun_with_retry` (entry) retries
      3× at 500ms but the previous `TunActor` hasn't been fully dropped before
      the new connection attempts to claim the same TUN name. Rapid
      connect/disconnect cycles accumulate stale connections, eventually causing
      resource exhaustion and process kill (OOM or SIGKILL). Needs proper TUN
      lifecycle tracking — ensure the old actor is fully dropped before allowing
      a new connection to reuse the name.
- [ ] No color in `[+]` notification messages — `format_notification()` in
      `cli/src/output.rs` emits plain text; should apply terminal colors
      conditionally (color support is already gated on `IsTerminal`).
- [ ] Log prefix inconsistency in REPL — mix of `warn:` prefix (from
      `tracing::warn!`) and `[+]`/`[-]`/`[!]` prefixes (from notifications).
      Consolidate into a consistent style. Broader fix: unified logging format
      — logfmt at most (e.g. `tracing-logfmt`) for `wallhackd`
      background/systemd (non-slim only), consistent prefixed format for
      foreground/REPL (slim always uses this). Watch bloat.
- [x] No connected message on exit node when a peer connects — entry logs
      `"Connected to {peer_addr}"` but exit connect-only mode does not.
- [x] REPL `connect` command doesn't resolve hostnames — passes raw address
      string to daemon without DNS resolution. `resolve_endpoint()` exists in
      daemon transport but REPL bypasses it. Results in `"invalid address:
      attacker"` when using hostnames.
- [ ] Noisy reconnect messages on exit node after entry exits — multiple
      overlapping messages ("Connection tasks died", "Transport disconnected",
      "Connection dropped") fire at non-verbose log levels. Consolidate into a
      single clean message.
- [ ] Website CLI docs still say "Ping the daemon or a peer" — should be
      "Ping a peer" (daemon ping was removed, `website/src/content/docs/cli.mdoc`
      line 15).

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

- [ ] Drop glibc release builds in favour of musl static only. Add
      `aarch64-unknown-linux-musl`. Consider armv7 for older 32-bit targets.

## Code Quality & Architecture

- [ ] Newtype wrappers for domain primitives — `PeerId`, `PeerName`, `Psk` are
      raw `String`; port numbers are raw `u16`. Introduce newtypes
      (`struct Psk(String)` with `ZeroizeOnDrop`, `struct PeerId(String)`) so
      the compiler prevents mixing, reduces ambiguous clone noise, and documents
      intent at the type level.
- [ ] `Metrics` field visibility — seven `AtomicU64` fields in
      `crates/core/src/control/metrics.rs` are `pub`. Make them private; the
      existing `inc_*`/`dec_*` methods are the correct API surface. Direct
      callers should not be able to `store(0)` or `fetch_add(arbitrary)`.
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
