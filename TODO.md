# Wallhack TODO

## Performance

- [ ] Reverse throughput asymmetry: `tcp_upstream` ~987 Mbps vs `tcp_downstream`
      ~188 Mbps (QUIC). Root cause: `poll_write` in entry-stack deposits data into
      smoltcp TX buffer then calls `notify_one`, deferring TUN emission to the next
      Tokio scheduler tick. Fix applied in `tcp_stream.rs` (call `inner.poll(now)`
      immediately before dropping lock). Pending benchmark verification.
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
- [ ] **Multiple simultaneous listen addresses** — `--listen :4433 --listen :443/tcp`
      lets a single daemon accept connections on multiple ports and transports
      concurrently. Useful when the exit node is behind a firewall that blocks UDP
      but allows 443/TCP: the exit picks whichever it can reach. Implementation:
      `ConnectivitySpec::Listen(Vec<AddressSpec>)`, spawn one tokio listener task
      per spec, `StatusResponse.listen_addr` → `repeated string listen_addrs` in
      proto. CLI: argh `Vec<String>` for repeated `--listen`. No known tunnel tool
      (e.g. ligolo) supports this.
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

- [ ] **Auto-routing not implemented** — entry node does not inject kernel routes for the
      exit peer's announced networks when a TUN is created. User must run
      `ip route add <cidr> dev <tun>` manually after every connect. This should be
      automatic and verified by the smoke test suite (add a smoke test that checks
      routes exist after connect).
- [ ] **`disconnect_peer` by address or connection ID** — `wallhack_disconnect_peer`
      only accepts a peer name. Unnamed peers (relays that don't propagate names) are
      impossible to disconnect. Need either address-based disconnect
      (`disconnect --addr 10.99.1.10:43006`) or a short connection ID shown in
      `wallhack peers` output that can be passed to `disconnect`.
- [ ] **Stale peer not cleaned up after remote restart** — when a peer process is
      killed and restarts (e.g. gateway-perimeter exit→relay), the old connection
      lingers in `wallhack peers` as a second entry for the same host alongside the
      new connection. Two peers for the same physical node is confusing and indicates
      the old session wasn't properly torn down.
- [ ] **`status=connected latency=— tun=false listen=false connect=false` is an
      impossible state** — `wallhack peers` shows a connected peer with all capability
      flags false and no latency. If `status=connected` then either `listen` or
      `connect` must be true (something initiated the connection). The flags likely
      reflect negotiated capabilities rather than connection direction; the display
      should either fix the semantics or add a `side=accept|connect` field (using
      wallhack's own terminology) so the state is not self-contradictory.
- [ ] **Latency not measured on connect** — `latency=—` on all peers until a manual
      `wallhack ping` is run. Latency should be sampled automatically on first connect
      (a single ping immediately after handshake) so `wallhack peers` always shows a
      real value. Ongoing periodic sampling (e.g. every 30s) would also help detect
      degraded links.
- [ ] **Relay peer name not propagated to entry** — when a relay node connects, the
      entry node sees it as an unnamed address (e.g. `10.99.1.10:48535`) rather than
      the relay's declared name. This breaks deterministic TUN naming
      (`peer_name_to_iface`) so TUN gets a random name instead of `wh{hash}`.
- [ ] **Relay peer role reported as `exit`** — `wallhack peers` shows relay peers
      as `role=exit`. Should report `role=relay`.
- [ ] **Stale TUN interfaces not cleaned up on disconnect** — when a peer disconnects
      (or restarts with a different role), the old TUN interface remains on the entry
      node and kernel routes pointing to it linger. Auto-cleanup on disconnect needed.
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

- [ ] **`--fixed-role` naming** — `--fixed-role relay` is confusing; "fixed" implies
      overriding something. Prefer `--role relay` (or just make role a positional
      subcommand). `--fixed-role` is used in static range setups as the normal way
      to set a role.
- [ ] **Relay `--listen` address underdocumented** — relay mode accepts both `--connect`
      (upstream) and `--listen` (for downstream peers) but neither the help text nor
      any docs explain the relay topology model, which interface the listener binds to,
      or how the chain is formed.
- [ ] **No relay startup confirmation log** — relay mode emits no clear log line
      like "relay listening on X, connected to Y via Z". Hard to know if startup
      succeeded without polling `wallhack status` separately.
- [ ] **No topology visibility** — from the entry node there is no way to see the
      full relay chain or which nodes are downstream of a relay. Add
      `wallhack topology` or `wallhack peers --recursive` to show the tree.
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

- [ ] **Version display: include git hash + build timestamp** — `wallhack --version`
      shows `0.6.2 (dev)` with no git SHA or build time. When running from initrds
      built from a dirty tree it's impossible to tell which binary is deployed without
      manual cross-referencing. Add short git SHA + ISO build timestamp (e.g.
      `0.6.2-dev (a1b2c3d, 2026-03-14T14:00Z)`). Critical for multi-VM ranges where
      different nodes may have different binary ages.
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
- [ ] **Field-threading anti-pattern** — six call sites thread individual fields
      from `ErasedConnectResult`/`ErasedAcceptResult` instead of passing the
      struct whole. The existing `ExitContext` in `exit.rs` is the correct
      pattern to follow. Priority order (by param count reduction):
      1. `auto::run_auto_accept_session_inner` 14 → ~3 (`ErasedAcceptResult` + `NodeResources` subset)
      2. `auto::run_auto_connect_session_dispatch` 12 → ~3 (`ErasedConnectResult` + `NodeResources` subset)
      3. `exit::run_exit_loop_inner` 9 → 3 (`ErasedConnectResult` + existing `ExitContext`)
      4. `entry::run_entry_connected_inner` 9 → ~4 (or collapse via existing `run_entry_connected_erased`)
      5. `relay::run_relay_loop_inner` 8 → ~3 (`ErasedConnectResult` + new `RelaySessionContext`)
      6. `entry::start_api` 8 → ~3 (new `ApiStartParams` wrapping `ApiConfig` + shared resources)
      Do after open PRs (#64/#65/#66) merge — those touch the same files.
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

## Range UAT (2026-03-16)

### Pontoon MCP / infrastructure findings

- [ ] **`pontoon mcp` stale config** — MCP server snapshots `pontoon.yml` at startup; edits (e.g. memory bumps) are invisible until Claude Code is restarted. `mcp__pontoon__range_up` silently uses old values. Workaround: run `~/.local/bin/pontoon -f range/pontoon.yml down && up` from CLI after any `pontoon.yml` edit.
- [ ] **MCP socket inode mismatch after `range_up`** — stale socket file from previous QEMU left on disk; new QEMU binds to a fresh inode, so filesystem path points to dead inode. `vm_exec` and `vm_console_stream` both return `Connection refused`. `lsof -p <qemu>` shows socket in LISTEN but connections refuse. Fix: CLI `pontoon down && up` fully clears `/tmp/pontoon/` state.
- [ ] **`vm_exec_bg` wedges serial console** — long-running or hung commands (nmap to unreachable host, `vm_exec_bg`) consume the serial console; subsequent `vm_exec` calls time out. `vm_pkill` also times out. Only recovery is CLI `pontoon down && up`.
- [ ] **`vm_restart` times out at 120 s** — reports failure but VM may actually be running; `range_status` shows it as alive with new PID while `vm_exec` refuses. Misleading. CLI restart is more reliable.
- [ ] **Attacker VM memory was 256 m — OOM after ~5 min** — attacker, web-external, corp-proxy, corp-socks, ssh-server, gateway-datacenter, db-mariadb, gold, intranet, fileserver, printer all died. Fixed: bumped attacker to 512 m. Other small VMs (64–128 m) may still be marginal under load.
- [x] ~~**`vm_exec` `cmd` vs `command` parameter**~~ — correct param is `command` (not `cmd`); documented.

### Wallhack bugs confirmed in UAT

- [ ] **Multi-hop pivot blocked by relay mode crash** — exit→relay pivot requires gateway-perimeter in relay mode (`--fixed-role relay --connect … --listen …`). Relay mode connects then immediately drops (`control_tx` dropped — see Bugs section above). Rapid reconnect then triggers TUN EBUSY storm on entry. Multi-hop pivot completely non-functional until relay mode is fixed.
- [ ] **TUN EBUSY from relay reconnect storm** — gateway-perimeter relay crash caused ~10 rapid reconnects; each one found `wh7a66aeb7` busy. Required manual `ip link delete wh7a66aeb7` on attacker to recover. Auto-cleanup on disconnect (see Bugs) would prevent this.
- [ ] **`punt!` TCP hang wedges console** — TCP to unreachable CIDR (10.99.3.x, 10.99.4.x, 10.99.5.x) blocks indefinitely (no RST). `nmap`/`nc` to mixed reachable+unreachable hosts hangs entire scan, wedging the pontoon console. **Never mix reachable and unreachable hosts in one nmap/nc call from attacker until punt! is fixed.**
- [ ] **`latency=—` always** — no latency shown on any peer without manual `wallhack ping`. Auto-ping on connect needed (see Bugs section).
- [ ] **`tun=false listen=false connect=false` with `status=connected`** — gateway-perimeter shows all capability flags false despite being a connected exit peer. Impossible/misleading state (see Bugs section).
- [ ] **Relay peer role reported as `exit`** — confirmed: gateway-perimeter connecting as relay shows `role=exit` on entry side.

### Wallhack UAT passes

- [x] **Auto-route announcement** — gateway-perimeter announces `10.99.2.0/24` on connect; kernel route `10.99.2.0/24 dev wh7a66aeb7` auto-installed on attacker. ✓
- [x] **`mcp__wallhack__status`** — role, version, uptime, listen addr, capabilities all correct. ✓
- [x] **`mcp__wallhack__peers`** — shows connected peers with addr, role, status. ✓
- [x] **`mcp__wallhack__routes`** — shows auto routes with peer name. ✓
- [x] **Office network reachable via TUN** — nmap to 10.99.2.0/24 from attacker returns correct hosts/ports. ✓

### Range recon (attacker → perimeter, direct)

- Perimeter hosts found: 10.99.1.10 (gateway-perimeter), .21 (ftp/vsftpd 3.0.5), .50 (squid :3128), .51 (dante socks5 :1080), .80 (nginx :80)
- web-external: HTML comment leaks `admin:admin123`
- corp-proxy (squid): open relay, reaches office network but NOT datacenter
- ftp-server: anonymous login untested (no `ftp` client on attacker)
- `nmap --no-stylesheet` works; `nmap -sV` fails (nse_main.lua missing — **initrd not rebuilt after adding `nmap-scripts` to attacker layer**)
- `curl` missing on attacker — **initrd not rebuilt after adding `curl` to attacker layer**
- Need `pontoon build` to bake in curl + nmap-scripts

### Range recon (attacker → office, via wallhack TUN)

- Office hosts found: 10.99.2.22 (ssh/OpenSSH 9.9), .80 (nginx intranet), .100 (samba 139/445), .200 (printer Flask app :5000)
- intranet: leaks `DB Host: 10.99.3.20`, creds `app / supersecret`
- printer: stub Flask app (`/` and `/jobs` only, no RCE)
- gateway-office (10.99.2.10): reachable, no open ports — router only
- ICMP ping through TUN hangs (punt! bug — ICMP not forwarded)

### Remaining UAT work

- [ ] **Multi-hop pivot** — relay mode fix landed (`fix/relay-mode`); needs UAT validation. Topology: ssh-server exit → gateway-perimeter relay → attacker entry.
- [ ] **Dynamic log levels** — `wallhack log <level>` REPL command + `POST /log-level` REST endpoint. Uses `tracing_subscriber::reload` Handle threaded into `EntryResources`. Key UAT value: flip to `debug` mid-session without restarting the daemon and losing the existing peer connection. Lets you inspect relay handshake, route announcement, and TUN lifecycle in real time.
- [ ] **Unprivileged EXIT node** — verify wallhack EXIT works as non-root. Currently only tested as root.
- [ ] **RSS check in `bench/check_bloat.sh`** — add slim EXIT RSS check (target: fits in 64 MB alongside a running service).
- [ ] **64 MB OOM on `vm_cp`** — 5 MB binary copy OOM-kills service in 64 MB VM. Test curl streaming into tmpfs instead.
- [ ] **Route persistence across restarts** — routes vanish on daemon restart; exit should re-announce on reconnect.
- [ ] **Services run as root** — all range services run as root. Add `su -s /bin/sh <user>` to pontoon stdlib start scripts for realism.
- [x] ~~**`pontoon build`**~~ — rebuilt initrds; curl + nmap-scripts baked into attacker layer. ✓
- [ ] **wallhack MCP vsock not compiled into initrd binary** — `wallhack-mcp` connects via `vsock://3:4434` but the binary deployed in the attacker initrd doesn't listen on vsock (only Unix socket). Needs rebuild with vsock feature enabled and `pontoon build` + cycle.

## Dropper

- [ ] Self-extracting payload format
- [ ] In-memory execution (no disk write)
- [ ] Polymorphic stub generation
- [ ] Anti-sandbox checks
- [ ] Hard mode cyber range (dropper deployment demo)

## Security Advisories

- [ ] AWS-LC: Timing side-channel in AES-CCM tag verification (high) — `aws-lc-fips-sys`, `aws-lc-sys`
- [ ] AWS-LC: `PKCS7_verify` certificate chain validation bypass (high) — `aws-lc-sys` GHSA-vw5v-4f2q-w9xf
- [ ] Quinn: unauthenticated remote DoS via panic in QUIC transport parameter parsing (high) — `quinn-proto` < 0.11.14, patched in 0.11.14


## Code Quality & QOL

- [ ] Remove `PskFailTracker` — replace with generic subscriber dedup by including IP in the log message. `PskFailTracker` is a per-IP HashMap in `daemon/src/mode/mod.rs`, used in `auto.rs` and `entry.rs`. The subscriber's consecutive-dedup handles the common case (single attacker hammering from one IP) just as well.

- [ ] `neli` pinned at `0.6` (`crates/daemon/Cargo.toml`) — 0.7.4 available. Likely a breaking API change; needs migration of `crates/daemon/src/netlink.rs`.

## Next batch: Phase 13f — Security Posture
- [ ] When any auth flag (`--psk`, `--cert`, etc.) is provided, automatically harden config: suppress auto-negotiation and auto-routing. See `docs/tasks/13f-security-posture.md` for full spec. Depends on 13c/13d/13e (all done).

## CLI
- [ ] `wallhack peers --json` — machine-readable output matching REST API shape, with `tun_name` field in `PeerInfo`. Non-slim feature (watch bloat). Needed by bench init.sh to discover TUN name dynamically instead of hardcoding FNV-1a hash.

## Website
- [ ] `website.just` file is in the wrong place?