# Phase 13a: Handshake & Proper Ping

Replaces the one-way `ExitNodeHello` with a bidirectional handshake exchange
so both sides of a connection know each other's identity, capabilities, and
authentication status before any tunnel traffic flows. Also wires up proper
periodic ping/pong with latency tracking.

This is the wire protocol foundation that every subsequent phase builds on.

**Design spec:** `docs/tasks/13-zero-config-and-friends.md`

---

## Scope

`crates/wire/proto/data.proto`,
`crates/wire/proto/control.proto`,
`crates/wire/proto/management.proto`,
`crates/core/src/transport/bridge.rs` (rename candidate — see item 6),
`crates/core/src/control/handler.rs`,
`crates/core/src/control/peers.rs`,
`crates/core/src/node_api.rs`,
`crates/daemon/src/mode/entry.rs`,
`crates/daemon/src/mode/exit.rs`,
`crates/daemon/src/mode/relay.rs`,
`crates/core/src/client/quic/mod.rs`,
`crates/core/src/client/ws/mod.rs`,
`crates/core/src/server/quic/mod.rs`,
`crates/core/src/server/ws/mod.rs`

---

## Terminology

- **Role** — entry, exit, relay, or indeterminate (introduced in Phase 13b).
  A node has a role. It is not "an entry node" — it is "a node in the entry
  role." Roles are dynamic and can change at runtime. The existing `NodeRole`
  type is the canonical representation.
- **Connectivity** — listener, connector, or both. Simply which of
  `--connect` / `--listen` the operator provided. Mirrors the existing
  `ConnectivitySpec` type in the Rust codebase. Not a preference or intent —
  just a fact about how the node is connected.
- **Handshake** — the message exchanged by both sides of a connection
  immediately after transport connects. Carries identity (name, version),
  capabilities (tun_capable), connectivity facts (listening, connecting),
  authentication (psk_proof), and topology metadata (routes, hint). The name
  reflects that this is the handshake message, not a description of a single
  concern.

---

## Items

### 1. Define `Handshake` protobuf message

**File:** `crates/wire/proto/data.proto` (message definition)
**File:** `crates/wire/proto/control.proto` (referenced in `ControlMessage`)

The `Handshake` message is defined in `data.proto` (alongside the existing
`Ping`, `Pong`, and soon-deprecated `ExitNodeHello`) and referenced from
`control.proto`'s `ControlMessage` envelope — the same pattern used today
for `ExitNodeHello`. These are two halves of the same change: the message
shape lives in `data.proto`, the framing lives in `control.proto`.

Replace `ExitNodeHello` as the identification mechanism. The new message
carries everything peers need to determine the topology.

```protobuf
message Handshake {
  bool tun_capable = 1;
  bool listening = 2;             // Started with --listen
  bool connecting = 3;            // Started with --connect
  string name = 4;               // Stable identifier (user-provided or auto-generated)
  string version = 5;            // For compatibility checks
  bytes psk_proof = 6;           // HMAC proof of PSK knowledge — see security note below
  repeated string routes = 7;    // CIDR notation, directly reachable networks
  optional RoleHint hint = 8;    // Operator hint: PREFER, EXCLUDE, or FIXED
}
```

**Security: PSK proof via TLS channel binding.** The PSK is never sent over
the wire. Instead, both sides prove knowledge of it by computing an HMAC
over the TLS channel binding and the other handshake fields:

```
psk_proof = HMAC-SHA256(psk, tls_channel_binding || serialized_handshake_fields)
```

The TLS channel binding is derived from `export_keying_material()` (RFC 9266,
`tls-exporter`), which both QUIC (TLS 1.3) and rustls support natively. This
value is unique per TLS session, which gives three properties:

- **No PSK leak** — an unauthenticated peer that connects learns nothing
  about the PSK. They receive an HMAC, not the key.
- **No replay** — the channel binding is different for every TLS connection,
  so a captured proof is useless on a different session.
- **No MITM relay** — a man-in-the-middle terminates TLS on both sides,
  producing two different channel bindings. A proof valid on one side is
  invalid on the other.

No clock sync, no nonce exchange, no extra round trip. Both sides compute
the proof independently from their local TLS session state before sending
the handshake message.

When `--psk` is not configured, `psk_proof` is empty (zero-length bytes).
A node that expects a PSK rejects any peer with an empty or invalid proof.
A node that does not expect a PSK ignores the field.

**PSK memory hygiene.** After the handshake exchange completes and the
proof has been validated, the plaintext PSK should be zeroized from memory
on the Rust side. The PSK is only needed to compute and verify the proof —
holding it longer than necessary is unnecessary exposure.

`ExitNodeHello` is removed from the proto file. No backward compatibility
with the old handshake is needed.

### 2. Update `ControlMessage` to carry `Handshake`

**File:** `crates/wire/proto/control.proto`

```protobuf
message ControlMessage {
  oneof message {
    wallhack.data.Handshake handshake = 1;  // was: hello
    wallhack.data.Ping ping = 2;
    wallhack.data.Pong pong = 3;
    ControlRequest control_request = 4;
    ControlResponse control_response = 5;
    Disconnect disconnect = 6;
  }
}
```

### 3. Bidirectional handshake exchange

Both sides send a `Handshake` message immediately after the transport
connection is established. Both sides send concurrently and wait to receive
the peer's `Handshake`. The exchange completes when both messages have been
delivered. The control stream supports concurrent send/receive (it's a bidi
stream over QUIC or WebSocket), so there is no deadlock risk and no need to
impose an artificial ordering.

Concurrent exchange is symmetric — neither side is "first". This aligns
with Phase 13c's design where both sides independently derive the topology
from the same combined picture.

After the exchange, each side has the full picture of both peers and can
independently derive the topology (Phase 13c).

### 4. Remove dead `NodeCapability` code

**Files:** `crates/core/src/node_api.rs`, `crates/core/src/control/peers.rs`,
`crates/wire/proto/management.proto`

The existing `NodeCapability` enum (`Exit | Relay`) in `node_api.rs` is dead
code — `set_relay_capability()` is defined but never called anywhere in the
codebase. `has_relay_capability` is always `false`.

Remove:
- `NodeCapability` enum from `node_api.rs`
- `has_relay_capability` field from `PeerInfo` in `peers.rs`
- `set_relay_capability()` method from `Registry` in `peers.rs`
- `NodeCapability` enum from `management.proto` (or repurpose — see below)

Replace with real handshake data from the peer stored in the `Registry`
per peer.

### 5. Periodic ping/pong with latency tracking

**Files:** `crates/core/src/transport/bridge.rs`,
`crates/daemon/src/mode/entry.rs`, `crates/core/src/control/peers.rs`

The `Ping` / `Pong` messages already exist in `data.proto` and are wired into
`ControlMessage`. The control stream message loop already handles
ping → pong response. What's missing:

- A periodic ping sender (e.g. every 10s) on each side of a connection
- Latency calculation from the pong timestamp
- Store last-known latency per peer in the `Registry`
- Surface latency in the `peers` REPL command output and management API

The ping channel plumbing (`register_ping_channel`, `ping_peer`) in `peers.rs`
exists but is over-engineered for what's needed. Simplify: the periodic loop
runs inside the control stream handler, stores latency directly in the
registry.

The REPL `ping <peer>` command should trigger an immediate one-shot ping
through the same control stream — send a `Ping`, wait for the `Pong`, report
latency. This uses the same code path as the periodic loop (same message
types, same latency calculation) but is request/response rather than
fire-and-forget. The existing `ping_peer()` method in `Registry` can be
simplified to send a one-shot request through the control stream rather than
the current channel-of-channels indirection.

### 6. Rename `bridge.rs`

**File:** `crates/core/src/transport/bridge.rs`

"Bridge" is a networking term with a specific meaning (L2 frame forwarding
between segments). This file contains protocol framing (length-delimited
protobuf read/write) and control stream message dispatch. Rename to something
accurate — e.g. `protocol.rs`, `framing.rs`, or `stream.rs`.

This is a mechanical rename. Update all `use` / `mod` references.

---

## Notes

- The `routes` and `hint` fields are defined in `Handshake` here but not
  acted on until their respective implementation phases. They are included
  in the message now so the wire format is complete from the start. Fields
  that are not yet populated are empty/absent — proto3 default behaviour.

---

## Tests

Every item must have corresponding unit tests. This is non-negotiable — the
handshake is the most security-sensitive code path and the foundation for all
subsequent phases.

- **Handshake serialisation round-trip** — construct a `Handshake`, encode,
  decode, assert all fields match.
- **Handshake concurrent exchange** — both sides send simultaneously, both
  receive. Verify exchange completes correctly. Test with artificial delay on
  one side to confirm no ordering dependency.
- **Malformed handshake rejection** — missing required fields, unknown
  connectivity value, oversized message. Verify clean error, not panic.
- **PSK proof validation** — correct PSK produces valid proof, wrong PSK
  produces invalid proof (rejected), empty proof when no PSK configured
  (accepted). Verify proof is bound to the TLS session — same PSK on a
  different connection produces a different proof.
- **Ping/pong latency** — send ping with known timestamp, receive pong, verify
  latency calculation. Test timeout behaviour (no pong received).
- **Periodic ping interval** — verify pings are sent at the configured
  interval (use `tokio::time::pause()` for deterministic testing).
- **One-shot REPL ping** — `ping <peer>` triggers an immediate ping through
  the control stream and returns measured latency. Verify it uses the same
  code path as periodic pings (same message types, same latency calculation).

---

## Acceptance Criteria

- `ExitNodeHello` is removed from the codebase
- Both sides of a connection exchange `Handshake` messages before tunnel
  traffic flows
- `wallhack peers` shows latency for each connected peer
- `wallhack ping <peer>` returns measured latency
- `NodeCapability` enum and `set_relay_capability()` no longer exist
- `bridge.rs` is renamed
- All tests pass, including the new handshake and ping tests
