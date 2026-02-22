# Wallhack TODO

## Performance

- [ ] Reverse throughput asymmetry: forward ~3500 Mbps vs reverse ~968 Mbps on
      symmetric `copy_bidirectional` bi-stream path. Investigate Quinn flow control
      defaults, poll loop wakeup latency under egress load, and mutex contention
      between smoltcp writes and the poll loop.
- [ ] Buffer pooling for UDP packets and TUN reads
- [ ] Reduce global lock contention in netstack
- [ ] `arc-swap` for route table and peer registry — both are read-heavy,
      write-rare; `arc-swap` gives wait-free reads on the data path vs the
      current `parking_lot::Mutex`
- [x] Bounded broadcast channels with tail drop — **see docs/tasks/07-broadcast-to-mpsc.md**

## Dropper

- [ ] Self-extracting payload format
- [ ] In-memory execution (no disk write)
- [ ] Polymorphic stub generation
- [ ] Anti-sandbox checks
- [ ] Hard mode cyber range (dropper deployment demo)

## Security

- [ ] Certificate pinning
- [ ] Encrypted config storage

## REST API

- [ ] Periodic latency measurement via control channel ping/pong

## REPL Commands

- [ ] `shell` — spawn shell over tunnel

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

## Testing

- [ ] Integration test for full pivot chain
- [ ] Fuzzing for protocol parsers
- [ ] Chaos testing (network partitions, latency)

## Auto-role negotiation

- [ ] Add `ServerHello` protobuf message so the accepting node announces its
      role to the connecting node immediately after the client Hello.
- [ ] Implement auto-role detection: `wallhack --connect <host>` (no subcommand)
      connects, receives the `ServerHello`, and adopts the complementary role
      automatically — entry if the server is exit/relay, exit if the server is
      entry. Peer name defaults to random (same as `exit` today).
- [ ] Drop the subcommand requirement when `--connect` is the only flag.

## UX

- [ ] Noisy reconnect messages on the exit node after the entry node exits —
      multiple overlapping messages ("Connection tasks died", "Transport
      disconnected", "Connection dropped") fire at non-verbose log levels.
      Consolidate into a single clean message.

## Zero-config & auth UX

- [ ] XDG config file (`~/.config/wallhack/config.toml` or similar) for persistent
      user preferences — e.g. `require_auth = true` to opt into enforced PSK/mTLS
      for users who always want auth and want a hard failure rather than a warning.

## Build / Config

- [ ] Drop glibc release builds in favour of musl static only. Add
      `aarch64-unknown-linux-musl`. Consider armv7 for older 32-bit targets.

## Code Quality & Architecture

- [ ] Newtype wrappers for domain primitives — `PeerId`, `PeerName`, `Psk` are raw
      `String`; port numbers are raw `u16`. Introduce newtypes (`struct Psk(String)`
      with `ZeroizeOnDrop`, `struct PeerId(String)`) so the compiler prevents mixing,
      reduces ambiguous clone noise, and documents intent at the type level.
- [ ] `TryFrom<ProtoNodeRole>` error type — `type Error = String` in
      `crates/wallhack/src/types.rs`. Replace with a proper `NodeRoleError` enum;
      raw `String` errors opt out of the type system and make `?` chains harder to
      reason about.
- [ ] `Metrics` field visibility — All six `AtomicU64` fields in
      `crates/wallhack/src/control/metrics.rs` are `pub`. Make them private; the
      existing `inc_*`/`dec_*` methods are the correct API surface. Direct callers
      should not be able to `store(0)` or `fetch_add(arbitrary)`.
- [ ] `run_control_loop` parameter object — Seven parameters (four of which are
      `Option<&Tx>`) in `crates/wallhack/src/transport/bridge.rs`. Group the channel
      handles into a `ControlLoopHandles` struct to enforce all-or-nothing wiring,
      improve call-site readability, and remove the `Option`-juggling inside the loop.
- [ ] `ControlLoopExit::Disconnect` reason — `Disconnect(String)` carries a raw
      reason string. Replace with a `DisconnectReason` enum so exhaustive match catches
      new variants and callers can react structurally rather than parsing strings.
- [ ] `ConnectionManager` decomposition — `crates/wallhack/src/entry/manager.rs`
      handles TCP accept, UDP forwarding, SYN proxy dispatch, UDP session GC, and exit
      response handling in one `select!` loop. Extract each concern into a focused
      type or async fn with a clean input/output contract.
- [ ] Deduplicate orchestrator session patterns — TCP and UDP `get-or-create` logic in
      `crates/wallhack/src/exit/orchestrator.rs` are near-identical structs apart from
      the protocol label. Extract a generic `get_or_create_session` helper.
- [ ] `ExitNodeResponse` construction boilerplate — The `ExitNodeResponse { pair: Some(pair), response: Some(…) }` struct literal is repeated throughout the exit
      orchestrator. Add a constructor or builder method to centralise the pair-wrapping.

## Misc

- [ ] Audit `#[allow(clippy::...)]` call sites — confirm each suppression is
      intentional and add a comment explaining why, or fix the underlying issue.
- [ ] We have some serious naming issues in regards to topology, and the use of
      directional wording such as in/out send/receive and up/down. We need to
      refactor files based on the naming conventions in the agents.md file
- [ ] Add `version` command to repl
- [ ] `--version` is way too verbose, maybe also check `--verhose` before outputting so much
- [ ] Add `version` to info output when running
- [ ] Info logs on startup are too verbose
      ```
      [+] Starting as exit node
      [+] Running in headless mode (no REPL).
      [+] Exit node starting as vm
      [+] Resolving 10.0.0.2:6565
      [+] Resolved as 10.0.0.2:6565
      ```