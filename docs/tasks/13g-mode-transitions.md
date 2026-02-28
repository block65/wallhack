# Phase 13g: Role Transitions & REPL Control

Roles are dynamic. A node may transition to a different role at runtime,
whether triggered by a topology change, a peer event, or an explicit operator
command via the REPL. This phase implements the transition protocol and the
REPL commands for runtime role control.

**Design spec:** `docs/tasks/13-zero-config-and-friends.md`
**Depends on:** Phase 13b (indeterminate), Phase 13c (auto-negotiation),
Phase 13d (hints)

---

## Scope

`crates/wire/proto/control.proto`,
`crates/wire/proto/data.proto`,
`crates/core/src/control/handler.rs`,
`crates/daemon/src/mode/mod.rs`,
`crates/daemon/src/mode/entry.rs`,
`crates/daemon/src/mode/exit.rs`,
`crates/daemon/src/mode/relay.rs`,
`crates/cli/src/repl.rs`,
`crates/cli/src/cli.rs`,
`crates/core/src/node_api.rs`

---

## Terminology

- **Role transition** — a change from one role to another at runtime.
  Announced to peers and propagated along the chain.
- **Chain** — the linear path from entry through relays to exit. Role
  transitions propagate along the chain. Each node only communicates with
  its immediate neighbours.

---

## Items

### 1. Define `RoleTransition` message

**File:** `crates/wire/proto/control.proto`

```protobuf
message RoleTransition {
  NodeRole new_role = 1;
}
```

No explicit ack message is needed. The peer's response is implicit — it
re-evaluates and, if its own role changes, sends its own `RoleTransition`
in turn. The originator observes the peer's new role through the normal
capability/ping exchange. This avoids a dedicated ack message type and the
temptation to build complex ack-waiting logic around it.

Add `RoleTransition` as a variant in `ControlMessage` (or as a broadcast
message type if a broadcast channel is introduced).

### 2. Transition protocol

Before switching role, a node sends a `RoleTransition` message to all
connected peers. Each peer:

1. Receives the `RoleTransition`
2. Re-evaluates its own role against the updated topology using the same
   negotiation logic (Phase 13c)
3. Adopts a new role or transitions to indeterminate
4. If its own role changed, sends its own `RoleTransition` to its peers

The originating node sends the transition, completes it locally, and does
not wait for acknowledgement. Data plane traffic continues uninterrupted —
the transition is a control plane event only.

Each peer receives the `RoleTransition`, re-evaluates, and either adopts
a new role or goes indeterminate. If a peer's own role changes, it sends
its own `RoleTransition` to its peers — this is how transitions propagate
along the chain. There is no explicit acknowledgement message.

**If things don't converge, nodes go indeterminate.** The protocol does not
try to resolve every edge case. If the topology is ambiguous after a
transition, affected nodes sit in indeterminate until the operator provides
a hint or the topology changes. This is by design — indeterminate is cheap,
spaghetti resolution logic is not.

**Possible outcomes for each peer:**

- **Compatible** — peer adopts a valid new role, transitions, announces its
  own transition to its peers.
- **No valid role** — peer transitions to indeterminate, holds transport
  connection open.
- **Ambiguous** — peer transitions to indeterminate. Transport maintained.

### 3. Self-healing chain resolution

When any node changes role, it announces to its immediate neighbours. Each
neighbour re-runs negotiation against the updated state. If the result is a
valid role, it adopts and announces in turn. The resolution propagates along
the chain in a single pass.

**Example from the design spec:**

Three TUN-capable nodes sitting in indeterminate. Operator provides
`--prefer entry` to Node A:

```
[A: indeterminate] — [B: indeterminate] — [C: indeterminate]
```

1. A resolves to entry, announces to B
2. B sees TUN-capable entry upstream → resolves to relay (has two peers),
   announces to C
3. C sees relay upstream → resolves to exit

The entire chain comes up from a single hint on a single node.

### 4. REPL role commands

**File:** `crates/cli/src/repl.rs`

New REPL commands for runtime role control:

| Command | Effect |
|---|---|
| `role` | Show current role |
| `role entry` | Request transition to entry role |
| `role exit` | Request transition to exit role |
| `role relay` | Request transition to relay role |
| `hint prefer <role>` | Apply prefer hint at runtime |
| `hint exclude <role>` | Apply exclude hint at runtime |
| `hint fixed <role>` | Apply fixed hint at runtime |
| `hint clear` | Remove all hints (runtime and startup) |

**`role <target>` semantics:** This is a shorthand for `hint fixed <target>`.
It applies a hard constraint and triggers re-negotiation with peers
immediately. Because it uses `fixed` semantics:

- The local node adopts the target role unconditionally (subject to the
  permanently-impossible check from Phase 13d — e.g. `role entry` on a
  non-TUN node is an error, not indeterminate).
- Peers are notified via `RoleTransition` and re-evaluate.
- If a peer cannot accommodate (e.g. both sides now want entry), the peer
  goes indeterminate — but the local node stays in its fixed role.
- This is "force this role" not "request this role". The local node does not
  go indeterminate because a peer disagreed. The fixed constraint wins
  locally; the peer resolves its own state independently.

**`hint` commands** apply hints that take effect on the next negotiation
cycle (or trigger one immediately if the node is in indeterminate). Unlike
`role <target>`, soft hints (`hint prefer`) can result in the local node
going indeterminate if the peer has a conflicting preference.

### 5. NodeApi extensions

**File:** `crates/core/src/node_api.rs`

Add to the `NodeApi` trait:

```rust
async fn set_role(&self, role: NodeRole) -> Result<(), Error>;
async fn set_hint(&self, hint: RoleHint) -> Result<(), Error>;
async fn clear_hints(&self) -> Result<(), Error>;
async fn current_role(&self) -> NodeRole;
```

These are the IPC surface for the REPL commands and REST API.

### 6. Slim binary behaviour

The slim binary receives hints via CLI arguments at startup (`--prefer`,
`--exclude-role`, `--fixed-role`) — the same flags as the full binary.

**Roles change. Preferences don't.** The slim binary fully participates in
role transitions — it changes role in response to peer events and topology
changes, exactly like the full binary. What it cannot do is change its
*preferences* at runtime. There is no REPL to issue `hint prefer entry` or
`hint clear`. The startup hints are fixed for the lifetime of the process.

- Receives `RoleTransition` messages, re-evaluates, transitions as needed
- Announces its own transitions when topology changes force them
- Startup hints are applied during the initial handshake and
  influence negotiation exactly as they do in the full binary
- If no hint was provided and the chain never resolves because one is needed,
  it remains in indeterminate indefinitely — connected, alive, and ready

**Exit conditions for the slim binary:** The slim binary exits only on:
- `SIGTERM` / `SIGINT` (explicit operator or OS shutdown)
- Unrecoverable errors (e.g. TUN creation fails after role resolution,
  transport bind failure at startup)
- Graceful shutdown initiated by a peer (via `Disconnect` control message)

The slim binary does **not** exit on:
- Indeterminate role (holds connections, waits)
- Peer disconnection (reconnects with backoff)
- Role transition failure (goes indeterminate, waits)

### 7. Peer disconnection triggers re-evaluation

When a peer disconnects (graceful or ungraceful), all remaining peers
re-evaluate their roles against the updated topology. This may cause:

- A node to transition from an active role to indeterminate (lost the peer
  that justified its role)
- A node to transition from indeterminate to an active role (the disconnection
  resolved an ambiguity)

On reconnection, the full handshake repeats and roles are
re-negotiated from scratch. No role state from the previous session is
assumed.

---

## Tests

### Transition protocol

- **Announce and propagate** — node transitions, sends `RoleTransition`,
  peer receives, re-evaluates. If peer's role changes, it sends its own
  `RoleTransition`. Verify both sides end up in consistent roles.
- **Peer re-evaluates** — transition results in a peer having no valid
  role. Verify peer transitions to indeterminate.
- **Non-convergence → indeterminate** — transition creates ambiguity.
  Affected nodes go indeterminate rather than trying to resolve.
- **Transport survives** — verify no transport disconnection during any
  transition.

### Chain propagation

- **Three-node resolution** — three indeterminate nodes, hint applied to one.
  Verify the entire chain resolves in a single propagation pass.
- **Partial resolution** — four nodes, hint resolves three but the fourth
  remains ambiguous. Verify the resolved three are operational and the fourth
  is indeterminate.

### REPL commands

- **`role` shows current role** — verify output for each role including
  indeterminate.
- **`role entry` triggers transition** — verify the node transitions and
  peers re-evaluate.
- **`hint prefer entry` at runtime** — verify the hint takes effect on the
  next negotiation cycle.
- **`hint clear` removes all hints** — node started with `--prefer entry`.
  Operator types `hint clear`. Verify the startup hint is removed and
  re-negotiation occurs without it.
- **`role entry` on non-TUN** — error, same as `--fixed-role entry` on a
  non-TUN node (permanently impossible).
- **`role entry` forces local role** — peer goes indeterminate but local
  node stays in entry (fixed semantics).

### Disconnection

- **Peer disconnect → re-evaluation** — entry peer disconnects. Node in exit
  role transitions to indeterminate.
- **Reconnection → fresh negotiation** — peer reconnects, full capability
  handshake repeats, roles re-negotiated from scratch.
- **Graceful shutdown frame** — node sends disconnect reason before closing.
  Peer transitions to indeterminate and begins reconnection.

### Slim binary

- **Participates in peer-initiated transitions** — slim node receives
  `RoleTransition` from peer, re-evaluates, transitions as needed.
- **Stays in indeterminate** — slim node with no hint and ambiguous topology
  remains in indeterminate indefinitely. Does not exit.
- **Slim exit conditions** — slim node exits on SIGTERM, not on indeterminate,
  not on peer disconnect.

---

## Acceptance Criteria

- `RoleTransition` protocol works between peers
- Non-convergence results in indeterminate, not complex resolution logic
- Role transitions propagate along the chain (self-healing)
- REPL `role` and `hint` commands work
- `role <target>` forces the role locally (fixed semantics)
- `hint clear` removes both startup and runtime hints
- `NodeApi` trait has role/hint methods
- Slim binary changes role at runtime but cannot change preferences at
  runtime (startup hints are fixed for lifetime of process)
- Slim binary exits only on signal or unrecoverable error
- Peer disconnection triggers role re-evaluation in remaining peers
- No transport disconnection occurs during any role transition
- All tests pass
