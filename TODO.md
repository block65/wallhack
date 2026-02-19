# Wallhack TODO

## Performance

See [docs/ANALYSIS.md](docs/ANALYSIS.md) for full analysis.

- [ ] Reverse throughput asymmetry: forward ~3500 Mbps vs reverse ~968 Mbps on
      symmetric `copy_bidirectional` bi-stream path. Investigate Quinn flow control
      defaults, poll loop wakeup latency under egress load, and mutex contention
      between smoltcp writes and the poll loop.
- [ ] Buffer pooling for UDP packets and TUN reads
- [x] Replace `Vec<u8>` with `bytes::Bytes` in hot paths
- [ ] Reduce global lock contention in netstack
- [x] Rate limiting / `Semaphore` for max connections

## Build Optimizations

- [x] `minimal` feature flag — drop env-filter regex for dropper builds (implemented via `SimpleSubscriber`)
- [x] Feature-gate rustyline/REPL (~106KB savings)

## Dropper

- [ ] Self-extracting payload format
- [ ] In-memory execution (no disk write)
- [ ] Polymorphic stub generation
- [ ] Anti-sandbox checks
- [ ] Hard mode cyber range (dropper deployment demo)

## Security

- [ ] mTLS authentication between nodes
- [ ] Certificate pinning
- [ ] Node authentication/authorization
- [ ] Encrypted config storage

## Phase 4 Refactor

See [docs/specs/PHASE4.md](docs/specs/PHASE4.md) for full spec.

- [x] Traffic profiles (`--mode scan` vs `--mode session`)
- [x] Status handshake (`SessionStatus` protobuf message)
- [x] Optimistic TCP mode for scanning
- [x] Connection rate heuristic ("RTFM" warning)

## REST API

- [ ] Periodic latency measurement via control channel ping/pong
- [x] OpenAPI spec
- [x] HTTP control plane for headless operation
- [x] Stats/metrics endpoints
- [x] Health checks
- [x] SSE for real-time events
- [x] Basic auth support
- [x] Peers endpoint (uses peer registry)
- [x] Route add/delete endpoints
- [x] Peer disconnect endpoint (uses peer registry)
- [x] Input validation (CIDR, peer ID)
- [x] DNS rebinding protection (Host header validation)
- [x] Security headers (CSP, X-Frame-Options, no-sniff, etc.)
- [x] CORS disabled (same-origin only)
- [x] TLS always required (HTTPS only)
- [x] Auth warning when not configured

## REPL Commands

- [ ] `shell` — spawn shell over tunnel
- [x] `ping` — show version and uptime
- [x] `stats` — bandwidth/latency metrics
- [x] `peers` — show connected nodes (uses peer registry)
- [x] `sessions` — list active sessions
- [x] `route add/del` — manage IP routing
- [x] `disconnect` — terminate peer connection (uses peer registry)
- [x] Implement actual route management (requires routing table abstraction)

## Transports

- [ ] Unified transport pipes: after smoltcp, TCP and UDP should use the same
      protobuf format and same pipes. Two parallel UDP paths exist (bi-stream and
      orchestrator/broadcast) causing bugs. Unify so the only difference is at the
      wire/TUN/smoltcp layer.
- [x] HTTP CONNECT and SOCKS5 proxy support (HTTPS_PROXY / ALL_PROXY / NO_PROXY env vars)
- [ ] DNS tunneling
- [ ] ICMP tunneling
- [ ] HTTP/2 multiplexing
- [ ] Domain fronting support
- [x] Default zero-config TLS for WebSocket transport, matching QUIC's
      self-signed cert behaviour
- [ ] deterministic tun addresses based on cpu id

## Testing

- [ ] Integration test for full pivot chain
- [ ] Fuzzing for protocol parsers
- [ ] Chaos testing (network partitions, latency)

## UX

- [x] **Consistent peers format**: `peers` command output should be identical
      across entry and exit node types.
- [x] **Table layout for CLI output**: tabular data (peers, sessions, routes)
      should use whitespace aligned columns, not ad-hoc arrow/dash formatting.
      No crazy ascii art.
- [x] **Merge peers and sessions**: on the entry node, peers and sessions are
      effectively the same thing. Merge into a single `peers` view with the TUN
      device as an optional column.
- [x] **Deprioritize TUN name**: the TUN device is an implementation detail.
      Show peer ID prominently; show TUN name only as a secondary detail (useful
      for correlating with `ip link` in bash).
- [x] **Route command syntax**: model after `ip route`, not legacy `route`. e.g.
      `route add 10.0.0.0/24 via <peer_id>` instead of positional args. "via"
      optional but understood.
- [x] **Consider `ip` command**: alias `ip route` to `route`. Future potential
      for `ip link` (interface discovery) once exit nodes can report interfaces.
- [x] **Better error messages**: route errors now say "via peer" instead of
      "via tun" and include the underlying OS error.
- [x] Default fingerprint hash when no `sha256:` prefix provided
- [x] Connection IDs for entry node error correlation
- [x] Deduplicate errors (show only once)
- [x] Non-retryable errors (fingerprint mismatch, auth failures)
- [x] Show peer ID in peers list; normalize IPv4-mapped IPv6 addresses
- [x] Human-readable uptime in peer list
- [x] Deduplicate peers list (use exit_id as peer ID)
- [x] Ping precision (3 decimal places)
- [x] Correlate sessions to peers (show peer address in session listing)
- [x] Ping on exit nodes
- [ ] ctrl-c'd on an entry node, and got this on exit node:
      `     wallhack> [+] Connection tasks died - transport disconnected
  Transport disconnected, reconnecting...
  Connection dropped, reconnecting in 1s...
  `

      Lots of overlap in the messages. Not even in verbose mode.

## Stability

- [x] **Reconnect loop crashes cyber range**: fixed with exponential backoff
      (1s to 30s) on exit node reconnects and `SessionManager` TUN reuse on
      the entry node.

## Build / Config

- [x] Fix feature flags: `styles` module shouldn't require default features.
      Made unconditional; `anstyle` is now a required dep.

## Cyberrange

- [x] Add memory and CPU constraints to prevent host crashes due to runaway
      processes.
