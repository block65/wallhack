# Phase 13c: Auto-Negotiation

The core logic that determines which role each node operates in, derived
automatically from the handshake exchange. No explicit role flag is required.
Both sides of a connection independently derive the same topology from the
combined capabilities using deterministic rules.

**Best-effort, not exhaustive.** Auto-negotiation is designed to handle the
common cases cleanly — TUN-capable node + non-TUN node, relay in the middle,
etc. It does not attempt to solve every possible topology puzzle. When the
answer is ambiguous, the node goes indeterminate and the operator can
resolve it with a hint. This is a deliberate design choice: simple
negotiation logic that normally works is better than complex logic that
tries to handle every edge case. Indeterminate is cheap. Spaghetti
resolution code is not.

**Design spec:** `docs/tasks/13-zero-config-and-friends.md`
**Depends on:** Phase 13a (handshake), Phase 13b (indeterminate role)

---

## Scope

`crates/daemon/src/daemon_config.rs`,
`crates/daemon/src/mode/mod.rs`,
`crates/cli/src/daemon_cli.rs`,
`crates/core/src/types.rs` (or new file for negotiation logic)

---

## Terminology

- **Connectivity** — listener, connector, or both. Which of `--connect` /
  `--listen` the operator provided. Advertised in the handshake.
  Mirrors the existing `ConnectivitySpec` type. Not a preference — a fact.
- **TUN-capable** — the process has `CAP_NET_ADMIN` (or equivalent root
  privilege) and can create a TUN interface. Assessed once at startup.
- **Chain** — the strictly linear topology from entry through zero or more
  relays to exit. Each node has at most two neighbours. There are no lateral
  connections and no loops. The topology is always a path graph:
  entry → [relay → ...] → exit.

---

## Items

### 1. TUN capability detection at startup

Assess whether the process has the privileges required to create a TUN
interface. This is checked once at startup and advertised in the `tun_capable`
field of the `Handshake` handshake. The check must give ground truth, not
guess.

**Linux:** Try opening `/dev/net/tun` with `O_RDWR` and immediately close it.
This is one syscall and gives the actual answer — it accounts for non-root
users with `CAP_NET_ADMIN`, root in a container without `CAP_NET_ADMIN`, and
any other edge case. Do not use `geteuid() == 0` — it gives false negatives
(non-root with capabilities) and false positives (root in restricted
containers).

**macOS:** Check for the `utun` device. The mechanism differs (utun is
kernel-managed, not `/dev/net/tun`), but the principle is the same — probe,
don't guess.

**Windows:** TUN interfaces on Windows require a third-party driver (e.g.
Wintun). TUN capability detection on Windows should check whether the driver
is available and the process has sufficient privileges to interact with it.
This can be deferred if Windows TUN support is not yet implemented, but the
`tun_capable` field should be `false` on Windows until it is.

The result is assessed once at startup and never changes for the lifetime of
the process.

### 2. Connectivity inference

**File:** `crates/cli/src/daemon_cli.rs`, `crates/daemon/src/daemon_config.rs`

When no subcommand is provided, the `listening` and `connecting` bools in the
`Handshake` handshake are set directly from the flags provided:

| Arguments provided         | `listening` | `connecting` |
|---|---|---|
| `--listen` only            | true        | false        |
| `--connect` only           | false       | true         |
| `--listen` and `--connect` | true        | true         |

This replaces the mandatory `entry` / `exit` / `relay` subcommand with a
single top-level command:

```
wallhack --connect host:6565
wallhack --listen :6565
wallhack --connect host:6565 --listen :6565
```

The deprecated subcommands (Phase 13b) continue to work as explicit role
selectors.

### 3. Negotiation logic — pure function

**New file or section in an appropriate existing file.**

The negotiation logic must be a **pure function** with this signature
(conceptual):

```rust
fn negotiate(local: &Handshake, peer: &Handshake) -> NegotiationResult
```

Where `NegotiationResult` is one of:
- `Resolved(NodeRole)` — unambiguous role determined
- `Indeterminate(reason: &'static str)` — ambiguous, needs hint or topology change

The function is deterministic: given the same inputs, both sides produce the
same result. It has no side effects, no I/O, and no access to global state.
This makes it trivially unit-testable.

**Rules (from the design spec):**

| Local | Peer | Result |
|---|---|---|
| TUN-capable, not both | Non-TUN, not both | Local = entry |
| Non-TUN, not both | TUN-capable, not both | Local = exit |
| Both (listening + connecting) | Any | Local = relay |
| TUN-capable, not both | TUN-capable, not both | Indeterminate — symmetric ambiguity |
| Non-TUN, not both | Non-TUN, not both | Indeterminate — no node can be entry |
| TUN-capable, not both | Both (relay) | Local = entry |
| Non-TUN, not both | Both (relay) | Local = exit |

("Both" means `listening = true, connecting = true`. "Not both" means only
one of `listening` or `connecting` is true.)

**Key rules:**

- **Relay is unambiguous.** A node with both `listening` and `connecting` is
  always relay, regardless of TUN capability or peer capabilities. No
  negotiation required — relay is determined by connectivity alone.
- **Peer of a relay.** When a non-relay node connects to a relay, it resolves
  its role based on its own TUN capability alone. A TUN-capable node becomes
  entry; a non-TUN node becomes exit. The relay's presence signals that the
  chain continues — the non-relay node does not need to wait for the full
  chain to be connected before resolving. This is the non-hint equivalent of
  `--prefer relay`'s early signal (Phase 13d).
- **Both non-TUN.** Two non-TUN nodes that are not relays both enter
  indeterminate — neither can create a TUN interface, so no entry is possible.
  This is a real scenario (two unprivileged processes) and the correct result
  is indeterminate, not an error.

### 4. Apply negotiation result

After the handshake exchange completes (Phase 13a), each side calls the
negotiation function with its own capability and the peer's capability. The
result determines the role transition:

- `Resolved(role)` — transition to that role, begin data plane operations
- `Indeterminate(reason)` — stay in indeterminate, log the reason, wait for
  topology change or hint (Phase 13d)

### 5. Relay reconnect loop (prerequisite fix)

The relay role currently has no reconnect loop for its upstream connection —
if the upstream peer drops, the relay exits. This is a known bug
(TODO.md: "Relay mode: no reconnect loop"). Auto-negotiation depends on
ordering independence, which depends on all connectivity types having
retry/reconnect behaviour.

**This must be fixed as part of Phase 13c or as a standalone pre-task.**
The fix is straightforward: apply the same exponential backoff retry pattern
that the exit connector already uses (`connect_loop`). The relay should
reconnect its upstream connection and re-run the handshake on
reconnection.

### 6. Ordering independence

Neither side needs to start first:
- A listener waits indefinitely for inbound connections
- A connector retries on exponential backoff with jitter until the peer
  becomes available

The chain converges to a connected state regardless of startup order. This
is partially implemented today (exit connector has a retry loop) and is
extended to all connectivity types in this phase (including relay upstream,
per item 5 above).

---

## Tests

The negotiation function is the single most important piece of code to test
in the entire auto-negotiation system. Because it is a pure function, testing
is straightforward.

### Exhaustive topology table

Unit test every row from the "Summary of Valid Topologies" table in the design
spec:

| Local TUN | Local L | Local C | Peer TUN | Peer L | Peer C | Expected local role |
|---|---|---|---|---|---|---|
| true | true | false | false | false | true | entry |
| false | false | true | true | true | false | exit |
| true | false | true | false | true | false | entry |
| false | true | false | true | false | true | exit |
| true | true | false | true | false | true | indeterminate |
| true | false | true | true | true | false | indeterminate |
| false | true | false | false | false | true | indeterminate |
| false | false | true | false | true | false | indeterminate |
| any | true | true | any | false | true | relay |
| any | true | true | any | true | false | relay |
| any | true | true | any | true | true | relay (relay-relay adjacency, multi-hop chain) |
| true | true | false | any | true | true | entry |
| true | false | true | any | true | true | entry |
| false | true | false | any | true | true | exit |
| false | false | true | any | true | true | exit |

(L = listening, C = connecting)

### Symmetry property

For every test case, verify that the peer independently derives the
complementary role. If local derives `entry`, peer must derive `exit` for the
same input pair (with local/peer swapped). If local derives `indeterminate`,
peer must also derive `indeterminate`.

### Determinism property

Call the function twice with the same inputs. Verify identical result. (This
is trivially true for a pure function but documents the contract.)

### Edge cases

- Both sides relay → both resolve to relay
- TUN-capable + relay peer → TUN-capable resolves to entry immediately
- Non-TUN + relay peer → non-TUN resolves to exit immediately
- Both non-TUN, neither relay → both indeterminate (no node can be entry)
- Connector with no peer available → stays in indeterminate, retries connection

### Integration-level

- Two nodes connect with complementary capabilities → both resolve to correct
  modes within the handshake timeout
- Two TUN-capable nodes connect → both stay in indeterminate, both log the
  reason

---

## Acceptance Criteria

- `wallhack --connect host:6565` (no subcommand) works — node auto-negotiates
  role
- `wallhack --listen :6565` (no subcommand) works
- `wallhack --connect host:6565 --listen :6565` auto-resolves to the relay role
- Two nodes with complementary capabilities auto-resolve without any role flag
- Two TUN-capable nodes both enter indeterminate with a clear log message
- The negotiation function exists as a pure, testable function with no I/O
- All topology table tests pass
- Symmetry property holds for all test cases
- Ordering independence: swapping which node starts first produces the same
  final topology
