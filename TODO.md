# Wallhack TODO

## Performance
See [docs/ANALYSIS.md](docs/ANALYSIS.md) for full analysis.
- [ ] Buffer pooling for UDP packets and TUN reads
- [ ] Replace `Vec<u8>` with `bytes::Bytes` in hot paths
- [ ] Reduce global lock contention in netstack
- [ ] Rate limiting / `Semaphore` for max connections

## Build Optimizations
- [ ] `minimal` feature flag - drop env-filter regex for dropper builds (~321KB savings)
- [ ] Feature-gate rustyline/REPL (~106KB savings)
- [ ] Feature-gate rcgen cert generation (~62KB savings)

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
- [ ] Traffic profiles (`--mode scan` vs `--mode session`)
- [ ] Status handshake (SessionStatus protobuf message)
- [ ] Optimistic TCP mode for scanning
- [ ] Connection rate heuristic ("RTFM" warning)

## REST API
- [x] HTTP control plane for headless operation
- [x] Stats/metrics endpoints
- [x] Health checks
- [x] SSE for real-time events
- [x] Basic auth support
- [x] Peers endpoint (uses peer registry)
- [x] Route add/delete endpoints (stub)
- [x] Peer disconnect endpoint (uses peer registry)
- [x] Input validation (CIDR, peer ID)
- [x] DNS rebinding protection (Host header validation)
- [x] Security headers (CSP, X-Frame-Options, no-sniff, etc.)
- [x] CORS disabled (same-origin only)
- [x] TLS always required (HTTPS only)
- [x] Auth warning when not configured
- [ ] Periodic latency measurement via control channel ping/pong
- [ ] OpenAPI spec
- [ ] Consider: API key auth as alternative to basic auth

## REPL Commands
- [x] `ping` - show version and uptime
- [x] `stats` - bandwidth/latency metrics
- [x] `peers` - show connected nodes (uses peer registry)
- [x] `sessions` - list active sessions
- [x] `route add/del` - manage IP routing (stub)
- [x] `disconnect` - terminate peer connection (uses peer registry)
- [ ] `shell` - spawn shell over tunnel
- [ ] Implement actual route management (requires routing table abstraction)

## Transports
- [ ] DNS tunneling
- [ ] ICMP tunneling
- [ ] HTTP/2 multiplexing
- [ ] Domain fronting support

## Testing
- [ ] Integration test for full pivot chain
- [ ] Fuzzing for protocol parsers
- [ ] Chaos testing (network partitions, latency)
