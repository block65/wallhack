# Wallhack TODO

## Performance

- [ ] Reverse throughput asymmetry: `tcp_upstream` ~987 Mbps vs `tcp_downstream`
      ~188 Mbps (QUIC). Root cause: `poll_write` in entry-stack deposits data
      into smoltcp TX buffer then calls `notify_one`, deferring TUN emission to
      the next Tokio scheduler tick. Fix applied in `tcp_stream.rs` (call
      `inner.poll(now)` immediately before dropping lock). Pending benchmark
      verification.
- [ ] Buffer pooling for UDP packets and TUN reads
- [ ] Reduce global lock contention in entry-stack
- [x] ~~Broadcast → mpsc migration on data path~~ — done.

## Relay

- [x] ~~**Bidi stream bridging**~~ — done (`feat(relay): bridge bidirectional
      streams`). Relay now bridges bidi streams between source and exit peers
      using `copy_bidirectional`. SYN probes and TCP data sessions work through
      relay chains.
- [x] ~~**Topology visibility**~~ — done: relay sends `PeerAnnouncement` over
      control stream; entry registers announced peers in its registry.
      `wallhack peers` on entry shows exit nodes behind relays.

## Transports

- [ ] DNS over tunnel — intercept DNS queries at the TUN interface on the exit
      node and route them back through the tunnel for resolution on the entry
      side, preventing DNS leaks on the target network that would otherwise be
      visible to IDS/monitoring. `hickory-resolver` (`dns-resolver` feature) is
      already present and would handle resolution on the entry side.
- [ ] DNS transport — encapsulate tunnel traffic inside DNS queries/responses to
      egress through firewalls that block all TCP/UDP except port 53.
- [ ] Full ICMP support — forward control messages (e.g., Time Exceeded,
      Parameter Problem) to support diagnostic tools like `traceroute` and
      enable Path MTU Discovery.
- [ ] ICMP as an egress transport — encapsulate the tunnel inside ICMP Echo
      packets to bypass firewalls that block all outbound TCP/UDP.
- [ ] **Multiple simultaneous listen addresses** — `--listen :4433 --listen
      :443/tcp` lets a single daemon accept connections on multiple ports and
      transports concurrently. Useful when the exit node is behind a firewall
      that blocks UDP but allows 443/TCP: the exit picks whichever it can reach.
      Implementation: `ConnectivitySpec::Listen(Vec<AddressSpec>)`, spawn one
      tokio listener task per spec, `StatusResponse.listen_addr` → `repeated
      string listen_addrs` in proto. CLI: argh `Vec<String>` for repeated
      `--listen`. No known tunnel tool (e.g. ligolo) supports this.
- [ ] HTTP/2 multiplexing
- [ ] Domain fronting support
- [ ] Deterministic TUN addresses based on peer identity

## Auto-negotiation

- [x] ~~**Tiebreaker for symmetric capabilities**~~ — done: interactive flag
      (human at terminal) breaks TUN-capable ambiguity. Relay accept-side
      Fixed(Entry) hint forces exit role on accepted peers. Both-interactive
      still Indeterminate — use `--prefer-role`.

## REPL

- [ ] `shell` — spawn shell over tunnel
- [ ] Per-peer traffic stats — `stats [<peer>]` showing bytes/packets per peer
      rather than global node aggregates. Requires per-peer counters in
      `Metrics`/`Registry`.
- [ ] Runtime mode promotion — e.g. promoting exit → relay via CLI command.
      Entry → relay is nonsensical. Could be `wallhack promote relay` or
      similar.
- [ ] Relay direction — how does a relay decide which direction to forward?
      Needs investigation.

## REST API

- [ ] `connect` and `listen` endpoints — expose `NodeApi::connect` and
      `NodeApi::listen` over the REST API so nodes can be managed
      programmatically (e.g. orchestration scripts, web dashboards).
- [x] ~~Periodic latency measurement via control channel ping/pong~~ — done:
      initial ping on connect + 30s heartbeat on all connection paths.

## Security

- [ ] Certificate pinning
- [ ] Encrypted config storage

## Testing

- [ ] Integration test for full pivot chain
- [ ] Fuzzing for protocol parsers
- [ ] Chaos testing (network partitions, latency)
- [x] ~~Benchmark scenario naming~~ — renamed `tcp_fwd` → `tcp_upstream`,
      `tcp_rev` → `tcp_downstream`. Both are measured from the entry node's POV
      (iperf3 client always on entry, server always on exit at `ECHO_PRIV`).
      Upstream = entry sends; downstream = entry receives (server sends via
      `-R`).

## Bugs

- [x] ~~**Auto-routing not implemented**~~ — done: `auto.rs` auto-installs
      kernel routes for peer-advertised CIDRs when TUN is created.
- [ ] **`disconnect_peer` by address or connection ID** —
      `wallhack_disconnect_peer` only accepts a peer name. Unnamed peers (relays
      that don't propagate names) are impossible to disconnect. Need either
      address-based disconnect (`disconnect --addr 10.99.1.10:43006`) or a short
      connection ID shown in `wallhack peers` output that can be passed to
      `disconnect`.
- [x] ~~**Stale peer not cleaned up after remote restart**~~ — fixed by
      connection ID–guarded `unregister_if_current` in PR #82.
- [x] ~~**`status=connected` with all capability flags false**~~ — fixed:
      `update_capabilities()` now called in exit connect mode.
- [x] ~~**Latency not measured on connect**~~ — done: initial ping after
      handshake + 30s heartbeat on all connection paths (entry, exit, auto).
- [x] ~~**Relay peer name not propagated to entry**~~ — fixed: relay extracts
      peer name from handshake instead of using raw socket address.
- [x] ~~**Relay peer role reported as `exit`**~~ — fixed: relay data plane
      wiring and `update_capabilities()` now correct.
- [x] ~~**Stale TUN interfaces not cleaned up on disconnect**~~ — fixed in PR
      #82: `delete_tun` runs after manager task abort+join; kernel auto-removes
      routes.
- [x] ~~TUN EBUSY on rapid reconnect~~ — fixed in PR #82:
      `SessionManager.get_or_create` preemptively deletes stale TUN;
      `run_connection_loop` aborts+joins manager task to release fd Arcs;
      `TunDropGuard` for panic safety.
- [x] ~~No color in `[+]` notification messages~~ — done, uses `nu-ansi-term`
      behind `repl` feature gate.
- [x] ~~**`ping` returns status info, not RTT**~~ — moot: `ping` command removed
      in v0.12.0.
- [x] ~~**Initial heartbeat latency delayed ~30s**~~ — fixed: microsecond
      timestamp resolution in v0.11.1.
- [ ] Log prefix inconsistency in REPL — mix of `warn:` prefix (from
      `tracing::warn!`) and `[+]`/`[-]`/`[!]` prefixes (from notifications).
      Consolidate into a consistent style. Broader fix: unified logging format —
      logfmt at most (e.g. `tracing-logfmt`) for `wallhackd` background/systemd
      (non-slim only), consistent prefixed format for foreground/REPL (slim
      always uses this). Watch bloat.
- [x] ~~Relay mode: `control_tx` dropped immediately~~ — fixed: `control_tx`
      retained across relay session lifetime.
- [x] ~~Relay mode: no peer registration~~ — fixed: `Arc<Registry>` threaded
      through relay; `register()`/`unregister()` called on connect/disconnect.
- [x] ~~Noisy reconnect messages on exit node~~ — done: consolidated to single
      `info!("Peer disconnected: {name}")`, details at debug level.
- [x] ~~Website CLI docs ping description~~ — updated to "Measure latency to a
      peer".

## UX

- [x] ~~**`--fixed-role` naming**~~ — done: `hint` command eliminated, unified
      into `role` command. `role entry` (hard), `role prefer entry` (soft),
      `role exclude entry`, `role auto` (clear). Daemon flags: `--role`,
      `--prefer-role`, `--exclude-role`.
- [ ] **Relay `--listen` address underdocumented** — relay mode accepts both
      `--connect` (upstream) and `--listen` (for downstream peers) but neither
      the help text nor any docs explain the relay topology model, which
      interface the listener binds to, or how the chain is formed.
- [ ] **No relay startup confirmation log** — relay mode emits no clear log line
      like "relay listening on X, connected to Y via Z". Hard to know if startup
      succeeded without polling `wallhack status` separately.
- [ ] **No topology visibility** — from the entry node there is no way to see
      the full relay chain or which nodes are downstream of a relay. Add
      `wallhack topology` or `wallhack peers --recursive` to show the tree.
- [ ] XDG config file (`~/.config/wallhack/config.toml` or similar) for
      persistent user preferences — e.g. `require_auth = true` to opt into
      enforced PSK/mTLS for users who always want auth and want a hard failure
      rather than a warning.
- [ ] Update the website with the benchmarks, explain that they are just "in the
      gigabits per second" and its kind of irrelevant because the tunnel isn't a
      bottleneck, its the OS or the VM. Confirm this makes sense first. Some
      benchmarks are below 1 Gbps, which should be quoted. Latency can be quoted
      also. Maybe we can just say "1 Gbps+". Its like a weird flex because we
      can't say.

## Build / Config

- [x] ~~**Version display: include git hash + build timestamp**~~ — done:
      semver+build-metadata format with git SHA and compact timestamp.
- [ ] Release process: `feat:` commits trigger a minor (0.x.0) bump, which per
      semver pre-1.0 signals breaking changes. Make `feat:` a patch bump while
      pre-1.0, reserve minor bumps for genuinely breaking changes.
- [ ] Drop glibc release builds in favour of musl static only. Add
      `aarch64-unknown-linux-musl`. Consider armv7 for older 32-bit targets.

## Code Quality & Architecture

- [ ] Newtype wrappers for domain primitives — `PeerId`, `PeerName`, `Psk` are
      raw `String`; port numbers are raw `u16`. Introduce newtypes (`struct
      Psk(String)` with `ZeroizeOnDrop`, `struct PeerId(String)`) so the
      compiler prevents mixing, reduces ambiguous clone noise, and documents
      intent at the type level.
- [x] ~~`Metrics` field visibility~~ — fields now private with `snapshot()`
      accessor returning `node_api::Metrics`.
- [x] ~~Redundant role conversion helper~~ — done: `proto_to_core_role()`
      already removed; `.into()` used everywhere.
- [ ] **Field-threading anti-pattern** — six call sites thread individual fields
      from `ErasedConnectResult`/`ErasedAcceptResult` instead of passing the
      struct whole. The existing `ExitContext` in `exit.rs` is the correct
      pattern to follow. Priority order (by param count reduction): 1.
      `auto::run_auto_accept_session_inner` 14 → ~3 (`ErasedAcceptResult` +
      `NodeResources` subset) 2. `auto::run_auto_connect_session_dispatch` 12 →
      ~3 (`ErasedConnectResult` + `NodeResources` subset) 3.
      `exit::run_exit_loop_inner` 9 → 3 (`ErasedConnectResult` + existing
      `ExitContext`) 4. `entry::run_entry_connected_inner` 9 → ~4 (or collapse
      via existing `run_entry_connected_erased`) 5.
      `relay::run_relay_loop_inner` 8 → ~3 (`ErasedConnectResult` + new
      `RelaySessionContext`) 6. `entry::start_api` 8 → ~3 (new `ApiStartParams`
      wrapping `ApiConfig` + shared resources) Do after open PRs (#64/#65/#66)
      merge — those touch the same files.
- [ ] `run_control_loop` parameter object — seven parameters (four of which are
      `Option<&Tx>`) in `crates/core/src/transport/bridge.rs`. Group the channel
      handles into a `ControlLoopHandles` struct to enforce all-or-nothing
      wiring, improve call-site readability, and remove the `Option`-juggling
      inside the loop. **See docs/tasks/10-type-system-improvements.md (item
      4).**
- [ ] `ControlLoopExit::Disconnect` reason — `Disconnect(String)` carries a raw
      reason string. Replace with a `DisconnectReason` enum so exhaustive match
      catches new variants and callers can react structurally rather than
      parsing strings. **See docs/tasks/10-type-system-improvements.md.**
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
- [ ] `start_api()` config bundling — `crates/daemon/src/mode/entry.rs` passes
      `metrics`, `peers`, `routes`, TLS config, username, and secret as separate
      arguments even though `EntryResources` already groups part of the state.
      Take a dedicated API config/resources object instead of threading sibling
      fields individually.
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

## Range UAT (2026-03-16)

### Pontoon MCP / infrastructure findings

- [ ] **`pontoon mcp` stale config** — MCP server snapshots `pontoon.yml` at
  startup; edits (e.g. memory bumps) are invisible until Claude Code is
  restarted. `mcp__pontoon__range_up` silently uses old values. Workaround: run
  `~/.local/bin/pontoon -f range/pontoon.yml down && up` from CLI after any
  `pontoon.yml` edit.
- [ ] **MCP socket inode mismatch after `range_up`** — stale socket file from
  previous QEMU left on disk; new QEMU binds to a fresh inode, so filesystem
  path points to dead inode. `vm_exec` and `vm_console_stream` both return
  `Connection refused`. `lsof -p <qemu>` shows socket in LISTEN but connections
  refuse. Fix: CLI `pontoon down && up` fully clears `/tmp/pontoon/` state.
- [ ] **`vm_exec_bg` wedges serial console** — long-running or hung commands
  (nmap to unreachable host, `vm_exec_bg`) consume the serial console;
  subsequent `vm_exec` calls time out. `vm_pkill` also times out. Only recovery
  is CLI `pontoon down && up`.
- [ ] **`vm_restart` times out at 120 s** — reports failure but VM may actually
  be running; `range_status` shows it as alive with new PID while `vm_exec`
  refuses. Misleading. CLI restart is more reliable.
- [ ] **Attacker VM memory was 256 m — OOM after ~5 min** — attacker,
  web-external, corp-proxy, corp-socks, ssh-server, gateway-datacenter,
  db-mariadb, gold, intranet, fileserver, printer all died. Fixed: bumped
  attacker to 512 m. Other small VMs (64–128 m) may still be marginal under
  load.
- [x] ~~**`vm_exec` `cmd` vs `command` parameter**~~ — correct param is
  `command` (not `cmd`); documented.

### Wallhack bugs confirmed in UAT

- [x] ~~**Multi-hop pivot blocked by relay mode crash**~~ — fixed: `control_tx`
      retained + relay data plane wiring corrected.
- [x] ~~**TUN EBUSY from relay reconnect storm**~~ — fixed: relay crash resolved
      + EBUSY guard in reconnect path.
- [ ] **`punt!` TCP hang wedges console** — TCP to unreachable CIDR (10.99.3.x,
  10.99.4.x, 10.99.5.x) blocks indefinitely (no RST). `nmap`/`nc` to mixed
  reachable+unreachable hosts hangs entire scan, wedging the pontoon console.
  **Never mix reachable and unreachable hosts in one nmap/nc call from attacker
  until punt! is fixed.**
- [x] ~~**`latency=—` always**~~ — fixed: auto-ping on connect + 30s heartbeat.
- [x] ~~**`tun=false listen=false connect=false` with `status=connected`**~~ —
  fixed.
- [x] ~~**Relay peer role reported as `exit`**~~ — fixed.

### Wallhack UAT passes

- [x] **Auto-route announcement** — gateway-perimeter announces `10.99.2.0/24`
  on connect; kernel route `10.99.2.0/24 dev wh7a66aeb7` auto-installed on
  attacker. ✓
- [x] **`mcp__wallhack__status`** — role, version, uptime, listen addr,
  capabilities all correct. ✓
- [x] **`mcp__wallhack__peers`** — shows connected peers with addr, role,
  status. ✓
- [x] **`mcp__wallhack__routes`** — shows auto routes with peer name. ✓
- [x] **Office network reachable via TUN** — nmap to 10.99.2.0/24 from attacker
  returns correct hosts/ports. ✓

### Range recon (attacker → perimeter, direct)

- Perimeter hosts found: 10.99.1.10 (gateway-perimeter), .21 (ftp/vsftpd 3.0.5),
  .50 (squid :3128), .51 (dante socks5 :1080), .80 (nginx :80)
- web-external: HTML comment leaks `admin:admin123`
- corp-proxy (squid): open relay, reaches office network but NOT datacenter
- ftp-server: anonymous login untested (no `ftp` client on attacker)
- `nmap --no-stylesheet` works; `nmap -sV` fails (nse_main.lua missing —
  **initrd not rebuilt after adding `nmap-scripts` to attacker layer**)
- `curl` missing on attacker — **initrd not rebuilt after adding `curl` to
  attacker layer**
- Need `pontoon build` to bake in curl + nmap-scripts

### Range recon (attacker → office, via wallhack TUN)

- Office hosts found: 10.99.2.22 (ssh/OpenSSH 9.9), .80 (nginx intranet), .100
  (samba 139/445), .200 (printer Flask app :5000)
- intranet: leaks `DB Host: 10.99.3.20`, creds `app / supersecret`
- printer: stub Flask app (`/` and `/jobs` only, no RCE)
- gateway-office (10.99.2.10): reachable, no open ports — router only
- ICMP ping through TUN hangs (punt! bug — ICMP not forwarded)

### Remaining UAT work

- [ ] **Multi-hop pivot** — relay mode fix landed (`fix/relay-mode`); needs UAT
  validation. Topology: ssh-server exit → gateway-perimeter relay → attacker
  entry.
- [ ] **Dynamic log levels** — `wallhack log <level>` REPL command + `POST
  /log-level` REST endpoint. Uses `tracing_subscriber::reload` Handle threaded
  into `EntryResources`. Key UAT value: flip to `debug` mid-session without
  restarting the daemon and losing the existing peer connection. Lets you
  inspect relay handshake, route announcement, and TUN lifecycle in real time.
- [ ] **Unprivileged EXIT node** — verify wallhack EXIT works as non-root.
  Currently only tested as root.
- [ ] **RSS check in `bench/check_bloat.sh`** — add slim EXIT RSS check (target:
  fits in 64 MB alongside a running service).
- [ ] **64 MB OOM on `vm_cp`** — 5 MB binary copy OOM-kills service in 64 MB VM.
  Test curl streaming into tmpfs instead.
- [ ] **Route persistence across restarts** — routes vanish on daemon restart;
  exit should re-announce on reconnect.
- [ ] **Services run as root** — all range services run as root. Add `su -s
  /bin/sh <user>` to pontoon stdlib start scripts for realism.
- [x] ~~**`pontoon build`**~~ — rebuilt initrds; curl + nmap-scripts baked into
  attacker layer. ✓
- [ ] **wallhack MCP vsock not compiled into initrd binary** — `wallhack-mcp`
  connects via `vsock://3:4434` but the binary deployed in the attacker initrd
  doesn't listen on vsock (only Unix socket). Needs rebuild with vsock feature
  enabled and `pontoon build` + cycle.

## Dropper

- [ ] Self-extracting payload format
- [ ] In-memory execution (no disk write)
- [ ] Polymorphic stub generation
- [ ] Anti-sandbox checks
- [ ] Hard mode cyber range (dropper deployment demo)

## Security Advisories

- [x] ~~AWS-LC: Timing side-channel in AES-CCM tag verification~~ — patched:
  `aws-lc-sys` at 0.38.0.
- [x] ~~AWS-LC: `PKCS7_verify` certificate chain validation bypass~~ — patched:
  `aws-lc-sys` at 0.38.0 (GHSA-vw5v-4f2q-w9xf).
- [x] ~~Quinn: unauthenticated remote DoS via panic in QUIC transport parameter
  parsing~~ — patched: `quinn-proto` bumped to 0.11.14 (PR #80).


## Code Quality & QOL

- [x] ~~Remove `PskFailTracker`~~ — done: subscriber dedup handles the common
  case; plain `tracing::warn!` with peer address is sufficient.
- [x] ~~single character variable names anti-pattern~~ — done: full codebase
      sweep shadowed all non-shadowed clones and renamed opaque abbreviations.
- [x] ~~`neli` pinned at `0.6`~~ — done: migrated to 0.7 (builder API, private
  fields, synchronous socket module).

### Channel sprawl refactor
- [ ] `ControlChannels` — 6-field struct, most `None`. Replace with
  Handler/Registry direct references. Control loop already has
  `Option<&Handler>` on server side; extend to client side.
- [x] ~~Eliminate `latency_tx`~~ — done in channel sprawl refactor.
- [x] ~~Eliminate `role_transition_tx`~~ — done in channel sprawl refactor.
- [x] ~~Deduplicate QUIC/WS client connect~~ — done (commit `624bc9c`).
- [x] ~~Deduplicate QUIC/WS server accept~~ — done (commit `afc7671`).
- [ ] IPC client: 3 channels → `IpcConnection` object with `request()` method
- [ ] Source/sink naming: replace `_tx`/`_rx` convention with `_source`/`_sink`
- [ ] `outgoing_rx` → `control_sink` or similar (oxymoron: receiving end of
  outgoing messages)

### Stale terminology (audit 2026-03-18)
- [x] ~~`StatusResponse` → `InfoResponse`~~ — done.
- [x] ~~`NodeStatus` → `NodeInfo`~~ — done.
- [x] ~~`fn status()` → `fn info()`~~ — done.
- [x] ~~`fn set_hint()` → `fn hint_set()`~~ — done.
- [x] ~~`fn clear_hints()` → `fn hint_set_auto()`~~ — done.
- [x] ~~`fn remove_route()` → `fn route_del()`~~ — done.
- [x] ~~`fn disconnect_peer()` → `fn peer_disconnect()`~~ — done.
- [x] ~~`fn ping_peer()` → `fn peer_ping()`~~ — moot: ping removed in v0.12.0.
- [x] ~~`SetHintParams` → `HintSetParams`~~ — done.
- [x] ~~`SetHintRequestBody` → `HintSetRequestBody`~~ — done.
- [x] ~~MCP "Remove a route" → "Delete a route"~~ — done.
- [x] ~~`downstream` in node_api.rs doc~~ — done.
- [x] ~~`client` variable in entry/session.rs, icmp.rs → `source`~~ — done.
- [x] ~~OpenAPI operationId consistency~~ — done (peerPing moot: ping removed).

## Next batch: Phase 13f — Security Posture
- [ ] When any auth flag (`--psk`, `--cert`, etc.) is provided, automatically
  harden config: suppress auto-negotiation and auto-routing. See
  `docs/tasks/13f-security-posture.md` for full spec. Depends on 13c/13d/13e
  (all done).

## CLI
- [x] ~~`wallhack peers --json`~~ — done: `--json` output matching REST API
      shape with `tun_name` field.

## Website
- [ ] `website.just` file is in the wrong place?