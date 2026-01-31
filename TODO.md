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
- [ ] HTTP control plane for headless operation
- [ ] Session management endpoints
- [ ] Stats/metrics endpoints
- [ ] Health checks
- [ ] OpenAPI spec

## REPL Commands
- [ ] `shell` - spawn shell over tunnel
- [ ] `route` - show/manage IP routing table
- [ ] `sessions` - list active sessions with stats
- [ ] `kill <session>` - terminate specific session
- [ ] `stats` - bandwidth/latency metrics
- [ ] `peers` - show connected nodes

## Transports
- [ ] DNS tunneling
- [ ] ICMP tunneling
- [ ] HTTP/2 multiplexing
- [ ] Domain fronting support

## Testing
- [ ] Integration test for full pivot chain
- [ ] Fuzzing for protocol parsers
- [ ] Chaos testing (network partitions, latency)
