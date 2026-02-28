# Phase 13b: Indeterminate Role & Terminology

Introduces indeterminate as a fourth role alongside entry/exit/relay and
establishes the canonical terminology: entry, exit, relay, and indeterminate
are **roles**, not node types.

**Design spec:** `docs/tasks/13-zero-config-and-friends.md`
**Depends on:** Phase 13a (handshake)

---

## Scope

`crates/core/src/types.rs`,
`crates/wire/proto/control.proto`,
`crates/wire/proto/management.proto`,
`crates/daemon/src/daemon_config.rs`,
`crates/daemon/src/mode/mod.rs`,
`crates/cli/src/daemon_cli.rs`,
`crates/core/src/node_api.rs`,
`crates/core/src/control/handler.rs`,
`crates/core/src/control/peers.rs`

---

## Terminology

This phase establishes the canonical terminology for the rest of the project:

- **Role** — the current function of a node in the chain: entry, exit, relay,
  or indeterminate. Role is a runtime property that can change.
- **Entry role** — TUN interface is active. Packets are read from the TUN and
  forwarded to peers.
- **Exit role** — Receiving encapsulated packets and realising them as
  syscalls on the local network.
- **Relay role** — Forwarding traffic between two peers. Does not interact
  with a TUN interface.
- **Indeterminate role** — Connected to one or more peers but role has not
  been resolved. No tunnel traffic is forwarded. This is the initial role
  after connection and the role a node returns to whenever its current role
  becomes invalid.
- **Node** — a running wallhack instance. A node has a role. It is not "an
  entry node" — it is "a node in the entry role" or "a node running as
  entry." Roles change at runtime.

Indeterminate is not an error state. It is a first-class role, equal in
standing to entry/exit/relay. A node can sit in indeterminate indefinitely
without consuming meaningful resources and will transition to an active role
the moment the topology allows it.

---

## Items

### 1. Add `Indeterminate` variant to `NodeRole`

**File:** `crates/core/src/types.rs`

```rust
// before
pub enum NodeRole {
    Entry,
    Relay,
    Exit,
}

// after
pub enum NodeRole {
    Indeterminate,
    Entry,
    Relay,
    Exit,
}
```

`NodeRole` keeps its name. The terminology rule is: entry/exit/relay/
indeterminate are **roles**, not node types. A node has a role — it is not
"an entry node", it is "a node in the entry role." Roles change at runtime.

### 2. Add `Indeterminate` to wire protocol enums

**File:** `crates/wire/proto/control.proto`

```protobuf
enum NodeRole {
  ROLE_UNKNOWN = 0;    // maps to Indeterminate
  ROLE_ENTRY = 1;
  ROLE_RELAY = 2;
  ROLE_EXIT = 3;
}
```

`ROLE_UNKNOWN` already exists and can map to `Indeterminate`. No new variant
needed in the proto — just update the Rust conversion to map `ROLE_UNKNOWN`
↔ `Indeterminate` instead of treating it as an error.

**File:** `crates/wire/proto/management.proto`

Add `NODE_ROLE_INDETERMINATE` or reuse `NODE_ROLE_UNSPECIFIED` with the same
mapping. The management proto currently collapses Relay → Exit — fix this to
report all four roles accurately.

### 3. Transport survives role events

Enforce the invariant: role changes never cause transport disconnection.
Disconnection occurs only for transport-layer reasons (network failure,
explicit shutdown).

When a role becomes invalid (e.g. the peer that justified the entry role
disconnects), the node transitions to indeterminate rather than tearing down
the connection. The transport layer (QUIC/WebSocket connection) remains up,
keepalives continue, and the node is immediately ready to resume when the
topology resolves.

**Audit and refactor.** The current code has no concept of indeterminate, so
role mismatches likely cause disconnection or process termination today. A
preliminary audit is needed before implementation to identify every code path
where a role conflict, peer disconnection, or unexpected handshake result
triggers a transport teardown. Each of these must be refactored to transition
to indeterminate instead. Known candidates:

- Connection accept paths in `server/quic/` and `server/ws/` that reject
  connections with unexpected roles
- The `connect_loop` in exit connector that may exit on role mismatch
- Any `ControlLoopExit::Disconnect` path triggered by a role-related condition
  rather than a transport failure

The audit should be done early in this phase to size the refactoring work
before committing to a timeline.

### 4. Indeterminate behaviour

In the indeterminate role:
- The transport connection stays up
- Control stream messages (ping/pong, handshake exchange) continue normally
- No tunnel traffic is forwarded — data plane is paused
- The node logs that it is indeterminate and why, at **INFO** level — this
  is an expected operational state (e.g. two TUN-capable nodes connecting),
  not an error. ERROR is reserved for actual failures (transport loss,
  malformed messages). WARN is acceptable if the indeterminate state persists
  beyond a configurable threshold (e.g. 30s with no resolution).
- The node is ready to transition to an active role the moment the topology
  allows it (Phase 13c provides the negotiation logic)

### 5. Subcommand handling

**File:** `crates/cli/src/daemon_cli.rs`

The `entry`, `exit`, `relay` subcommands continue to work in this phase with
no deprecation warning yet. They remain the only way to set a role until
Phase 13c (auto-detection from `--connect`/`--listen`) and Phase 13d
(`--fixed-role`) land.

**Deprecation is deferred to Phase 13d.** The subcommands become equivalent
to `--fixed-role entry` / `--fixed-role exit` / `--fixed-role relay` once
that flag exists. Emitting a deprecation warning before the replacement is
available would be confusing — the warning would reference a flag that
doesn't work yet.

### 6. Update REPL `info` output

The `info` command should show the current role, including `indeterminate`
when applicable. Currently it shows the static role from startup config.
Update to reflect the runtime role which may differ from the startup config
once auto-negotiation is active.

---

## Tests

- **Indeterminate variant** — `NodeRole::Indeterminate` serialises to/from
  proto correctly. Round-trip through both `control.proto` and
  `management.proto` wire formats.
- **Transport survives role transition** — simulate a role becoming invalid
  (e.g. entry peer disconnects). Verify the transport connection remains
  open and the node transitions to indeterminate.
- **No data plane in indeterminate** — node in indeterminate does not
  forward any tunnel traffic. Verify that packets sent to the data channel
  are not delivered.
- **Control plane in indeterminate** — node in indeterminate still
  responds to ping, still participates in handshake exchange, still
  sends/receives control messages.
- **Role display** — `info` output correctly shows `indeterminate` when the
  node is in that role.

---

## Acceptance Criteria

- `NodeRole` has four variants including `Indeterminate`
- `ROLE_UNKNOWN` maps to `Indeterminate` in proto conversion (not treated as
  error)
- Management proto reports all four roles accurately (relay no longer
  collapses to exit)
- No code path disconnects a transport due to a role event
- `wallhack info` shows current runtime role including `indeterminate`
- Subcommands `entry`, `exit`, `relay` continue to work (deprecation deferred
  to Phase 13d when `--fixed-role` is available)
- All tests pass
