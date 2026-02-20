# Wallhack TODO

## Performance

- [ ] Reverse throughput asymmetry: forward ~3500 Mbps vs reverse ~968 Mbps on
      symmetric `copy_bidirectional` bi-stream path. Investigate Quinn flow control
      defaults, poll loop wakeup latency under egress load, and mutex contention
      between smoltcp writes and the poll loop.
- [ ] Buffer pooling for UDP packets and TUN reads
- [ ] Reduce global lock contention in netstack

## Dropper

- [ ] Self-extracting payload format
- [ ] In-memory execution (no disk write)
- [ ] Polymorphic stub generation
- [ ] Anti-sandbox checks
- [ ] Hard mode cyber range (dropper deployment demo)

## Security

- [ ] Certificate pinning
- [ ] Node authentication/authorization
- [ ] Encrypted config storage

## REST API

- [ ] Periodic latency measurement via control channel ping/pong

## REPL Commands

- [ ] `shell` — spawn shell over tunnel

## Transports

- [ ] Unified transport pipes: after smoltcp, TCP and UDP should use the same
      protobuf format and same pipes. Two parallel UDP paths exist (bi-stream and
      orchestrator/broadcast) causing bugs. Unify so the only difference is at the
      wire/TUN/smoltcp layer.
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

## Misc

- [ ] Audit `#[allow(clippy::...)]` call sites — confirm each suppression is
      intentional and add a comment explaining why, or fix the underlying issue.
