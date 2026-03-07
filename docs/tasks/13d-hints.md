# DONE
# Phase 13d: Hints

When auto-negotiation reaches an ambiguous state (typically two TUN-capable
peers with identical profiles), the operator provides a hint rather than an
explicit role flag. Hints are the lightest possible intervention — they nudge
negotiation toward a resolution without overriding it entirely.

**Design spec:** `docs/tasks/13-zero-config-and-friends.md`
**Depends on:** Phase 13c (auto-negotiation)

---

## Scope

`crates/wire/proto/data.proto`,
`crates/cli/src/daemon_cli.rs`,
`crates/daemon/src/daemon_config.rs`,
`crates/core/src/types.rs` (or wherever the negotiation logic lives after 13c)

---

## Items

### 1. Define `RoleHint` in protobuf

**File:** `crates/wire/proto/data.proto`

```protobuf
enum HintLevel {
  HINT_LEVEL_UNSPECIFIED = 0;
  HINT_LEVEL_PREFER = 1;     // Soft — yield if topology makes another node better
  HINT_LEVEL_EXCLUDE = 2;    // Medium — remove one role from consideration
  HINT_LEVEL_FIXED = 3;      // Hard — this role and no other
}

message RoleHint {
  HintLevel level = 1;
  NodeRole target = 2;       // The role being preferred, excluded, or fixed
}
```

Add the `RoleHint hint` field to the `Handshake` message (field 7, reserved
in Phase 13a).

### 2. CLI flags

**File:** `crates/cli/src/daemon_cli.rs`

All three hint levels use the same `--<level> <role>` pattern:

| Flag | Hint level | Effect |
|---|---|---|
| `--prefer <role>` | PREFER | Soft — take this role if contested, yield if topology makes another node unambiguously better |
| `--exclude-role <role>` | EXCLUDE | Medium — remove one role from consideration entirely |
| `--fixed-role <role>` | FIXED | Hard — this role and no other (see permanently/temporarily impossible distinction in item 3) |

Examples:
- `--prefer entry` — take entry if two TUN-capable nodes contest
- `--prefer relay` — signal relay early, before second peer connects
- `--exclude-role entry` — never be entry, even if TUN-capable
- `--fixed-role exit` — always exit, no negotiation

### 3. Extend negotiation logic

**File:** wherever `negotiate()` lives after Phase 13c

The pure negotiation function gains a hint parameter:

```rust
fn negotiate(
    local: &Handshake,
    peer: &Handshake,
) -> NegotiationResult
```

The `Handshake` already contains the optional `RoleHint`. The rules:

- **No ambiguity** — hints have no effect. The topology is already
  unambiguous.
- **Ambiguity + PREFER** — the node with the prefer hint takes the preferred
  role. If both sides have prefer hints for the same role, both stay
  indeterminate (conflicting preferences don't resolve).
- **Ambiguity + EXCLUDE** — the excluded role is removed from consideration.
  Negotiation continues between remaining roles. If no valid role remains,
  indeterminate.
- **FIXED** — no negotiation at all. The node adopts the fixed role
  unconditionally. Two failure cases must be distinguished:
  - **Permanently impossible** — the node lacks a capability that will never
    change at runtime (e.g. `--fixed-role entry` on a non-TUN node). This is
    a **startup error**, not indeterminate. The node cannot ever fulfil this
    role, so waiting is misleading. Fail fast with a clear message:
    `"--fixed-role entry requires TUN capability (CAP_NET_ADMIN)"`.
  - **Temporarily impossible** — the node has the capability but the current
    topology doesn't support it (e.g. `--fixed-role entry` but the peer is
    also TUN-capable with `--fixed-role entry`). This is indeterminate —
    the topology may change (peer disconnects, new peer connects) and the
    role may become valid later.

### 4. `--prefer relay` early signal

A node advertising `PREFER relay` in its handshake tells connecting
peers that the chain is expected to continue through it. This is useful in
partial topology situations — it allows negotiation to proceed directionally
before all peers are present, rather than both sides sitting in indeterminate
waiting for a third node.

### 5. Deprecated subcommands as `--fixed-role`

The deprecated `entry` / `exit` / `relay` subcommands (Phase 13b) are
equivalent to `--fixed-role <role>`. This makes the migration path clear:
`wallhack entry --listen :6565` behaves identically to
`wallhack --listen :6565 --fixed-role entry`.

The deprecation warning (deferred from Phase 13b to this phase, since
`--fixed-role` now exists) should make the implication explicit:

```
warning: 'entry' subcommand pins the role to entry (equivalent to --fixed-role entry).
         To auto-negotiate, use 'wallhack --listen :6565' instead.
```

This ensures users migrating from the old CLI understand they are opting out
of auto-negotiation by using a subcommand.

### 6. Interaction with secure posture (Phase 13f)

Phase 13f specifies that any auth flag (`--psk`, `--ca`,
`--accept-fingerprint`) activates hardened defaults, which suppress
auto-negotiation. Soft hints (`--prefer`) require auto-negotiation to
function.

**If `--prefer <role>` is provided alongside an auth flag without
`--auto-negotiate` or `--zero-config`, this is a startup error:**
`"--prefer requires --auto-negotiate or --zero-config under secure posture"`.
No silent promotion to `--fixed-role`, no silent ignore.

To use `--prefer` with auth:
```
wallhack --connect host:6565 --psk abc123 --auto-negotiate --prefer entry
```

`--fixed-role` and `--exclude-role` work under any posture — they are
explicit constraints, not soft negotiation hints.

This cross-phase interaction is specified here and in Phase 13f (item 4).

---

## Tests

### Hint resolution

- **PREFER breaks ambiguity** — two TUN-capable nodes, one has
  `--prefer entry`. That node resolves to entry, peer resolves to exit.
- **PREFER has no effect when unambiguous** — TUN-capable listener + non-TUN
  connector with `--prefer exit` on the TUN side. Node still resolves to
  entry (topology is unambiguous, hint ignored).
- **Conflicting PREFER** — both sides prefer entry. Both stay indeterminate.
- **EXCLUDE removes a role** — TUN-capable node with `--exclude-role entry`.
  Node cannot be entry regardless of peer capabilities.
- **EXCLUDE leaves no valid role** — node excludes its only valid role.
  Indeterminate.
- **FIXED overrides** — `--fixed-role exit` on a TUN-capable node. Node
  becomes exit even though it could be entry.
- **FIXED + permanently impossible** — `--fixed-role entry` on a non-TUN
  node. Startup error, not indeterminate.
- **FIXED + temporarily impossible** — `--fixed-role entry` on a TUN-capable
  node whose peer also has `--fixed-role entry`. Indeterminate — the topology
  may change.
- **PREFER relay early signal** — node with `--prefer relay` connects to a
  TUN-capable listener. Listener resolves to entry (not indeterminate)
  because the peer's relay preference signals that entry is needed upstream.

### Secure posture interaction

- **`--psk` + `--prefer entry`** — without `--auto-negotiate`, startup error.
- **`--psk` + `--prefer entry` + `--auto-negotiate`** — prefer hint takes
  effect, negotiation is active, auth still enforced.
- **`--psk` + `--fixed-role exit`** — works fine, no `--auto-negotiate`
  needed. `--fixed-role` is compatible with any posture.
- **`--psk` + `--exclude-role entry`** — works fine.

### Startup validation

- **`--fixed-role entry` on non-TUN** — startup error, clear message.
- **`--fixed-role relay` without both `--listen` and `--connect`** — startup
  error (relay requires both).

### Symmetry preservation

For every hint test case, verify that the peer independently derives the
complementary role (same symmetry property as Phase 13c).

---

## Acceptance Criteria

- `--prefer <role>` flag accepted
- `--exclude-role <role>` flag accepted
- `--fixed-role <role>` flag accepted
- Hints resolve previously-ambiguous topologies (e.g. two TUN-capable nodes)
- Hints have no effect on unambiguous topologies
- Deprecated subcommands behave as `--fixed-role`
- All hint test cases pass
- Symmetry property holds for all hint scenarios
