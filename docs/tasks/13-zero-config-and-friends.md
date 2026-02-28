# Wallhack Auto-Negotiation Protocol

## Overview

Wallhack determines the role of each node automatically based on two inputs: the local capability of the host it is running on, and the network arguments provided at startup. No explicit mode flag is ever required. Role is not static — it is a runtime property that can change as the chain topology evolves.

All negotiation and announcement happens over the existing protobuf control plane. The data plane carrying tunnel traffic is separate and unaffected by control plane events.

The user only ever needs to think in terms of network topology: am I connecting, listening, or both?

---

## Capabilities

Each node has a fixed capability profile that is assessed at startup and advertised to peers. Capabilities never change for the lifetime of the process.

**TUN-capable** — the process has `CAP_NET_ADMIN` (or equivalent root privilege) and is able to create a TUN interface. This is a prerequisite for acting as an entry node.

**Non-TUN-capable** — the process does not have the necessary privileges to create a TUN interface. This node can act as an exit or relay but never as an entry.

---

## Startup Behaviour

### Local role inference

Before any peer connection is established, the node infers its intended posture from the arguments it was given:

| Arguments provided | Inferred posture |
|----|---|
| `--listen` only | Listener — wait for inbound connections |
| `--connect` only | Connector — attempt outbound connection with retry |
| `--listen` and `--connect` | Relay — connect outbound and accept inbound simultaneously |

**Relay is unambiguous.** A node that is both connecting and listening is always a relay. No negotiation is required to determine this.

### Ordering independence

Neither side needs to start first. A listener waits indefinitely. A connector retries on an exponential backoff with jitter until the peer becomes available. The chain converges to a connected state regardless of startup order.

---

## Node States

A node is always in one of the following states:

| State | Meaning |
|---|---|
| **Indeterminate** | Connected to one or more peers but role has not yet been resolved. No tunnel traffic is forwarded. This is the initial state after any connection is established and the state a node returns to whenever its current role becomes invalid. |
| **Entry** | TUN interface is active. Packets are being read from the TUN and forwarded to peers. |
| **Exit** | Receiving encapsulated packets from an entry peer and realising them as syscalls. |
| **Relay** | Forwarding traffic between an upstream and a downstream peer. |

The underlying transport connection is never torn down due to a role event. Disconnection only occurs due to actual network failure or explicit operator shutdown. Role conflicts and ambiguities cause a transition to Indeterminate, not a disconnect. This preserves the transport layer across firewall state changes, NAT keepalives, and topology shifts where re-establishing the connection would be difficult or impossible.

---

## Role Negotiation

Once a connection is established, both sides concurrently send a `Handshake` message before any tunnel traffic flows. Neither side waits for the other — both send and then wait to receive. The exchange completes when both messages have been delivered.

```protobuf
message Handshake {
  bool tun_capable = 1;
  bool listening = 2;             // Started with --listen
  bool connecting = 3;            // Started with --connect
  string name = 4;               // Stable identifier (user-provided or auto-generated)
  string version = 5;            // For compatibility checks
  bytes psk_proof = 6;           // HMAC proof of PSK knowledge — never plaintext
  repeated string routes = 7;    // CIDR notation, directly reachable networks
  optional RoleHint hint = 8;    // Operator hint: PREFER, EXCLUDE, or FIXED
}
```

The PSK is never sent over the wire. The `psk_proof` field carries an HMAC-SHA256 over the TLS channel binding and the other handshake fields, proving knowledge of the pre-shared key without revealing it. The channel binding is derived from `export_keying_material()` (RFC 9266), which is unique per TLS session — preventing replay and MITM relay attacks without clocks or nonces.

The full picture of both peers is available to each side once the exchange completes. Each side independently derives the topology from the combined handshake data using the rules below. Routes announced here are used by the entry node to automatically populate its TUN routing table. See Route Announcement below.

### Entry

A node becomes entry when:
- It is TUN-capable, and
- Its peer is not TUN-capable, and
- The local posture is Listener or Connector (not Relay)

The entry node creates the TUN interface and begins injecting and reading packets.

### Exit

A node becomes exit when:
- It is not TUN-capable, or
- Its peer is TUN-capable and the local node did not advertise TUN capability

The exit node receives encapsulated packets from the entry node and realises them as syscalls on the local network.

### Relay

A node becomes relay when:
- It was started with both `--connect` and `--listen`

The relay node forwards encapsulated traffic between its upstream peer (toward entry) and its downstream peer (toward exit). It does not interact with a TUN interface at any point.

### Both peers TUN-capable

If both sides of a connection are TUN-capable, neither side can unambiguously take the entry role. Both nodes transition to Indeterminate. The transport connection is maintained. Both nodes log the reason and wait. Resolution occurs when the operator provides a hint, or when the chain state changes such that the symmetry is broken (e.g. a third peer connects with a distinct capability profile).

---

## Mode Transitions

Role is dynamic. A node may transition to a different role at any time, subject to the constraints below.

### Transition announcement

Before switching role, a node broadcasts a `RoleTransition` message to all connected peers on the broadcast channel:

```protobuf
message RoleTransition {
  Role new_role = 1;
}
```

The originating node sends the transition, completes it locally, and does not wait for acknowledgement. Data plane traffic continues uninterrupted throughout — the transition is a control plane event only.

### Peer response to a transition

Each peer receives the `RoleTransition` broadcast and re-evaluates its own role against the updated chain state using the same negotiation rules as at connection time. If the new role is valid the peer transitions. If not, the peer transitions to Indeterminate and holds the transport connection open. If a peer's own role changes as a result, it sends its own `RoleTransition` to its peers in turn.

**Possible outcomes for each peer:**

- **Compatible** — peer adopts a valid new role, transitions, announces its own transition to its peers.
- **No valid role** — peer transitions to Indeterminate, holds transport connection open and waits for the chain state to change.
- **Ambiguous** — multiple peers could take the same role. All affected peers transition to Indeterminate. Transport connections maintained. Resolution occurs when a hint is provided or the topology shifts.

### Transition examples

**Entry switches to exit:**

1. Entry broadcasts transition to exit and completes the transition locally.
2. Each peer evaluates whether it can become entry.
3. A single TUN-capable peer promotes to entry.
4. Non-TUN-capable peers have no valid role — they transition to Indeterminate and hold their transport connections open.
5. If more than one TUN-capable peer exists, all transition to Indeterminate (ambiguity). Transport connections are maintained and re-negotiation triggers when the state changes.

**Exit switches to entry:**

1. Exit broadcasts transition to entry and completes the transition locally.
2. Existing entry evaluates: it now has a TUN-capable peer that wants to be entry.
3. Existing entry may demote to exit if it is willing.
4. If existing entry does not support demotion, both nodes transition to Indeterminate. Both hold their transport connections. Re-negotiation is attempted when a hint arrives or further topology change occurs.

**Relay transitions to exit:**

1. Relay drops its listen socket — the downstream peer transitions to Indeterminate (transport maintained).
2. Relay broadcasts transition to exit toward its upstream peer.
3. Upstream peer re-evaluates. If upstream is entry, the topology is now valid (entry–exit).

---

## Transport vs Role

The transport layer (TCP/UDP connection between peers) and the role layer are explicitly separate concerns. Role events never cause transport disconnection. This is a deliberate design decision:

- Firewall state and NAT mappings are expensive to re-establish. Dropping the connection because of a role conflict would make recovery far harder than necessary on a compromised host.
- A node in Indeterminate is still reachable, still exchanging keepalives, and still capable of immediately resuming tunnel traffic once a valid role is negotiated — without any reconnection delay.
- Role negotiation is cheap. Reconnection is not.

**Indeterminate** is therefore a first-class operational state, not an error state. A node can sit in Indeterminate indefinitely without consuming meaningful resources, and will transition to an active role the moment the chain state allows it.

## Disconnection

Disconnection occurs only for transport-layer reasons:

### Graceful shutdown

A node that is shutting down sends a `Disconnect` control message before closing:

```protobuf
message Disconnect {
  DisconnectReason reason = 1;
}

enum DisconnectReason {
  DISCONNECT_REASON_UNSPECIFIED = 0;
  DISCONNECT_REASON_SHUTDOWN = 1;
  DISCONNECT_REASON_OPERATOR = 2;
}
```

Peers receiving this transition to Indeterminate and begin reconnection attempts using the standard backoff strategy.

### Ungraceful disconnection

If a connection is lost without a shutdown frame (network failure, process kill, firewall drop), peers detect the loss via keepalive timeout and treat it identically to a graceful shutdown.

### Reconnection

On reconnection the full handshake is repeated and roles are re-negotiated from scratch. A node that was exit before disconnection may become entry after reconnection if the chain state has changed in the interim. No role state from the previous session is assumed or carried over.

---

## Hints

Zero-config resolves correctly in the majority of topologies. When it cannot — specifically when two peers have identical capability profiles and neither can be automatically preferred — the operator provides a hint rather than an explicit mode flag.

Hints operate at three levels of increasing firmness:

| Hint | Level | Effect |
|---|---|---|
| `--prefer <role>` | Soft | This node takes the preferred role if contested, yields if topology makes another node unambiguously better |
| `--exclude-role <role>` | Medium | Removes one role from consideration entirely, negotiation continues between the remaining roles |
| `--fixed-role <role>` | Hard | No negotiation, this role and no other — Indeterminate is preferred over violation |

Hints are only acted on when the automatic negotiation reaches an ambiguous state. In unambiguous topologies they have no effect.

`--prefer relay` is particularly useful for partial topology situations. A node advertising relay preference in its handshake tells connecting peers that the chain is expected to continue through it, allowing negotiation to proceed directionally before all peers are present.

`--exclude-role` is useful when a node is TUN-capable but should never act as entry regardless of what peers connect to it — for example a pivot host that has root but sits too deep in the target network to be a sensible entry point. It differs from `--fixed-role` in that negotiation still occurs between the remaining roles.

`--fixed-role` is the hardest constraint and is OPSEC-critical in certain deployment scenarios. See the OPSEC section below.

---

## Route Announcement

Route announcement extends the handshake to include network reachability information. Each node advertises the routes it can reach, and the entry node automatically installs those routes into its TUN routing table. No manual `ip route` configuration is required on the attacker side.

### How it works

On connection and role resolution, each node announces the networks directly reachable from its local interfaces. These are included in the handshake frame alongside TUN capability and posture. The entry node receives these announcements and adds the corresponding routes to the TUN, pointing them toward the peer that announced them. Wallhack only ever adds or removes routes on TUN interfaces it owns — it never touches routes belonging to other interfaces. Auto-routing is best-effort; if a route add fails (prefix collision, permission denied, etc.), skip it and log a warning.

Route announcements propagate along the chain. A relay receives route announcements from its downstream peer and rebroadcasts them upstream toward entry as a `RouteUpdate`, with the relay itself set as the next hop. The relay does not merge or aggregate routes from multiple downstream peers — each announcement is forwarded independently, preserving the originating peer identity for withdrawal purposes. By the time the entry node has a fully resolved chain it has a complete picture of every network reachable through the tunnel without any operator configuration.

### Dynamic route updates

Routes follow the same lifecycle as roles and are communicated over the same control plane primitives:

- **On connection** — initial routes are announced in the `Handshake` exchange
- **On role change** — a `RouteUpdate` broadcast is sent alongside the `RoleTransition` broadcast with any changed routes
- **On transition to Indeterminate** — a `RouteWithdraw` broadcast is sent; entry removes the affected routes from the TUN routing table immediately
- **On reconnection** — the full `Handshake` exchange repeats and routes are re-announced from scratch, no prior state assumed

This keeps the TUN routing table consistent with the current state of the chain at all times. As you pivot deeper and add relays and exits, new networks appear in your routing table automatically. When a node drops off, those routes are withdrawn cleanly.

### Suppressing route announcement

Route announcement is part of zero-config behaviour and is suppressed when secure posture is active, along with auto-negotiation. The granular flags for surgical control:

| Flag | Effect |
|---|---|
| `--no-announce-routes` | This node does not advertise its local routes to peers |
| `--no-accept-routes` | Entry node does not automatically install announced routes into the TUN |

`--no-announce-routes` is useful when you want the tunnel but do not want to leak internal network topology in the handshake. `--no-accept-routes` is useful when you want to manage the attacker-side routing table manually rather than having it populated automatically.

### The TUN routing table as a live engagement map

A side effect of auto-routing is that the entry node's TUN routing table becomes a real-time map of everything reachable through the current chain. As the engagement progresses and the chain grows, the table grows with it. Removing a node from the chain removes its routes. The routing table always reflects the current reachable state, not a static configuration written at the start of the engagement.

---

## Security Posture

### Default posture

By default Wallhack uses TLS for encryption with no certificate verification. Connections are encrypted against passive interception but not authenticated — a rogue peer that can reach the listener could connect. For many engagements this is an acceptable tradeoff in exchange for zero configuration friction.

### Secure posture

Providing any authentication flag triggers a hardened posture as a bundle:

| Flag | Effect |
|---|---|
| `--psk <key>` | Pre-shared key authentication |
| `--ca <path>` | Mutual TLS — verify peer certificates against this CA bundle |
| `--accept-fingerprint <fp>` | Pin expected peer certificate fingerprint |

When any of these flags is present:
- The specified verification is enforced — connections that fail verification are rejected
- Auto-negotiation is suppressed — `--fixed-role` becomes the default
- Auto-promotion is disabled — a node will not take a role it was not started with

The rationale is that an operator who cares enough about authentication almost certainly also cares about the OPSEC implications of auto-negotiation. The hardened posture bundles both concerns so neither has to be remembered separately.

### Re-enabling zero-config behaviour in secure posture

When any auth flag is present, both auto-negotiation and auto-routing are suppressed. To re-enable the full zero-config experience alongside verified authentication, provide `--zero-config` explicitly:

```
./wallhack --connect attacker:6565 --psk abc123 --zero-config
```

If you want finer control, the individual flags still work:

| Flag | Effect |
|---|---|
| `--auto-negotiate` | Re-enables role negotiation only, routes still manual |
| `--auto-routes` | Re-enables route announcement and injection only, role still fixed |
| `--zero-config` | Re-enables both — shorthand for `--auto-negotiate --auto-routes` |

`--zero-config` is an explicit acknowledgement that the OPSEC tradeoffs of both systems are understood and accepted. Its presence in a command line makes that decision visible and auditable rather than implicit.

---

## OPSEC Considerations

Auto-negotiation is safe and desirable in most scenarios. There are specific situations on target networks where it can be actively dangerous.

**Unintended TUN interface creation**

If a TUN-capable node acting as exit loses its entry peer and a new non-TUN peer connects, auto-negotiation will promote it to entry and create a TUN interface. On a target host this means:

- A new network interface appears — visible to EDR, auditd, and any kernel-level monitoring
- The routing table changes — visible to network monitoring tools
- The host begins originating tunnel traffic rather than just handling syscalls — a completely different traffic signature

**Unintended listeners**

If a node auto-promotes to relay, a new inbound port opens on the target host. This can be detected by port scans, firewall anomaly detection, or the blue team simply noticing something listening that should not be.

**Mitigation**

Any node deployed on a target network should use `--fixed-role` unless auto-negotiation has been explicitly considered and accepted. When any auth flag is present this is the default. When running in zero-config mode on a target it must be specified manually:

```
./wallhack --connect attacker:6565 --fixed-role exit
```

This is the one configuration decision that cannot be made for the operator automatically — only the operator knows the sensitivity of the host the binary is running on.

---

## Slim Binary

The slim build omits the REPL and IPC but accepts the same CLI flags as the full binary, including all hint flags (`--prefer`, `--exclude-role`, `--fixed-role`). The auto-negotiation logic is identical — the slim binary participates in the same handshake and role negotiation as the full binary, and behaves identically with respect to Indeterminate: it holds transport connections open and waits for the chain state to resolve, exactly as the full binary does. It does not exit on Indeterminate.

Roles change. Preferences don't. The slim binary fully participates in role transitions — it changes role in response to peer events and topology changes, exactly like the full binary. The only behavioural difference is that preferences cannot be changed at runtime — there is no REPL to issue `hint prefer entry` or `hint clear` commands. Startup hints are fixed for the lifetime of the process. If no hint was provided and the chain never resolves because one is needed, the slim binary simply remains in Indeterminate indefinitely — connected, alive, and ready to resolve the moment a peer's state changes and makes the topology unambiguous.

This makes the slim binary suitable for scripted or blind deployment where arguments are known ahead of time and interactive intervention is not available.

---

## Chain Topology

Wallhack always forms a **chain topology** — a strictly linear path from entry through zero or more relays to exit. Each node has at most two neighbours: one upstream and one downstream. There are no lateral connections between nodes at the same level and no loops.

This is a deliberate constraint, not a limitation. A chain maps directly to the mental model a penetration tester already has when pivoting through a network. It is also simpler to reason about, simpler to secure (each node knows only its immediate neighbours, never the full path), and sufficient for every tunnelling and pivoting scenario the tool is designed for.

The term mesh does not apply. A mesh implies arbitrary cross-node connectivity. Wallhack's topology is always a path graph: entry → [relay → relay → ...] → exit.

---

## Self-Healing

Because transport connections are never dropped on role events and Indeterminate is a first-class state, the chain is self-healing by design.

When any node changes state — whether by resolving from Indeterminate to an active role, receiving a hint, or broadcasting a role transition — it announces that change to its immediate neighbours. Each neighbour re-runs the same negotiation logic against the updated state. If that produces a valid role, it adopts it and announces in turn. The resolution propagates along the chain in a single pass.

**Example: hint resolves a fully Indeterminate chain**

Three TUN-capable nodes are connected but all sitting in Indeterminate because no entry can be determined:

```
[Node A] — Indeterminate — [Node B] — Indeterminate — [Node C]
```

The operator provides `--prefer entry` to Node A. Node A resolves to entry and announces to Node B. Node B sees a TUN-capable entry upstream and resolves to relay (it has both an upstream and a downstream peer). Node B announces to Node C. Node C sees a relay upstream and no TUN capability requirement — it resolves to exit. The entire chain comes up from a single hint on a single node.

**Example: partial connectivity**

If two nodes cannot reach each other directly but both maintain connections to a third node, they remain Indeterminate on the unreachable link but fully operational on their other links. The chain degrades gracefully to whatever subset of the topology is currently resolvable rather than collapsing entirely.

**Example: node rejoins after disconnection**

A relay drops off due to network failure. Entry and exit transition to Indeterminate on the affected links but hold their transport connections where they can. When the relay reconnects, it re-runs the handshake, both neighbours re-evaluate, and the chain resolves again automatically. No operator intervention required.

The self-healing property is emergent — it falls out naturally from the combination of Indeterminate as a persistent waiting state, role announcements on every state change, and deterministic negotiation logic. No explicit recovery mechanism is needed.

---

## Summary of Valid Topologies

| Node A | Node B | Result |
|---|---|---|
| TUN-capable listener | Non-TUN connector | A = entry, B = exit |
| Non-TUN connector | TUN-capable listener | A = exit, B = entry |
| TUN-capable connector | Non-TUN listener | A = entry, B = exit |
| TUN-capable listener | TUN-capable connector | Ambiguous — both Indeterminate until hint provided or topology changes |
| Connect+listen (any) | Any | A = relay, B negotiates entry or exit with further peers |

---

## What the Operator Never Needs to Know

- The names of the internal modes
- Which binary to use for which role
- What order to start nodes in
- Any concept of "agent" vs "host" or equivalent

The operator only needs to know: connect, listen, or both.

---

## Implementation Phases

The auto-negotiation protocol is implemented across seven sequential phases (13a–13g).
Each phase is self-contained, testable, and shippable independently. The dependency
chain is strictly linear — no phase can be started until its prerequisites are complete.

```
13a  Handshake & Proper Ping
 │
 ├─► 13b  Indeterminate Role & Terminology
 │    │
 │    ├─► 13c  Auto-Negotiation
 │    │    │
 │    │    ├─► 13d  Hints
 │    │    │    │
 │    │    │    └─► 13f  Security Posture (also needs 13e)
 │    │    │
 │    │    └─► 13e  Route Announcement
 │    │
 │    └─► 13g  Mode Transitions & REPL (also needs 13c, 13d)
```

### Phase summary

| Phase | What | Touches | Design doc |
|-------|------|---------|------------|
| **13a** | Replace `ExitNodeHello` with bidirectional `Handshake` exchange. PSK proof via TLS channel binding. Ping/pong with latency tracking. Remove dead capability code. | wire protos, protocol.rs, mode/\*, control/peers | `13a-capability-handshake.md` |
| **13b** | Add `NodeRole::Indeterminate`. Transport survives role events. Audit all disconnect-on-role-conflict paths. | types.rs, wire protos, daemon\_config, mode/mod | `13b-indeterminate-mode.md` |
| **13c** | Deterministic pure function: `negotiate(local, peer) → NegotiationResult`. Both sides derive the same result from the same inputs. | New negotiation module, CLI flags, daemon\_config | `13c-auto-negotiation.md` |
| **13d** | `--prefer`, `--exclude-role`, `--fixed-role` hints. Extend `negotiate()`. Deprecate subcommands. | CLI, wire protos (RoleHint), negotiate fn | `13d-hints.md` |
| **13e** | Peers advertise reachable CIDRs. Entry TUN routing table auto-populated. Withdraw on disconnect. | wire protos (RouteUpdate/Withdraw), routes.rs, mode/entry, mode/relay | `13e-route-announcement.md` |
| **13f** | Auth flags suppress auto-negotiate + auto-routes. `--zero-config` re-enables. | CLI, daemon\_config resolution logic | `13f-security-posture.md` |
| **13g** | Runtime role transitions via `RoleTransition` messages. REPL commands. Slim binary behaviour. | wire protos, control/handler, mode/mod, REPL, node\_api | `13g-mode-transitions.md` |

---

## Execution Order (All Tasks)

The 13x phases do not exist in isolation. Several foundational tasks must be
interleaved to avoid building on known architectural weaknesses. The ordering
below accounts for all identified tasks across `docs/tasks/` and `TODO.md`.

### Tier 0 — Complete (already done)

- **01** Trivial cleanup
- **09** Remove broken JSON output

### Tier 1 — Foundation fixes (no dependencies, interleave freely)

Small, independent fixes that reduce risk for everything downstream. Do these
between 13x phases or in parallel — they never block each other.

| Task | Size | What | Why now |
|------|------|------|---------|
| **02** | XS | Protobuf safety — `vec_to_sized_array` returns `Result`; port truncation `try_from` | Silent truncation bugs in wire handling |
| **03** | S | Security hardening — dedup `SkipServerVerification`, validate CA roots, constant-time auth | 13a adds PSK proof; auth paths must be solid |
| **05** | M | Async correctness — double readiness, missing timeouts, wrong mutex, hot-path clones | Silent hangs affect every transport path |
| **10** | M | Type system — newtypes (`PeerId`, `Psk`, `PeerName`), `DisconnectReason` enum, private `Metrics` fields | Compiler-enforced domain primitives before more code uses raw strings |
| **08** | M | Task lifecycle — `JoinSet` adoption, 30s shutdown drain, fix fire-and-forget spawns | Silent panics, no graceful shutdown |

### Tier 2 — Architectural cleanup + 13x interleaved

Ordered sequence. Dependencies are noted.

| # | Task | Depends on | Rationale |
|---|------|-----------|-----------|
| 1 | **13a** Handshake | — | **Done.** Foundation for all 13x phases. |
| 2 | **relay-reconnect** bug fix | — | **Done.** Relay now reconnects upstream on disconnect. |
| 3 | **13b** Indeterminate role | 13a | **Done.** Fourth role variant, data plane paused, transport survives. |
| 4 | **12** Delete dual TCP path B | — | Remove dead orchestrator TCP path. Less code to touch in 07 and 11. |
| 5 | **07** Broadcast → mpsc | 12 | **Critical correctness fix.** Silent packet loss on data path under load. Must land before 13e/13g. |
| 6 | **13c** Auto-negotiation | 13b, relay-reconnect | Pure function, testable in isolation. Core negotiation logic. |
| 7 | **11** Transport dedup | 12 | Kill ~500 lines QUIC/WS copy-paste in mode files before 13d–13g add more mode code. |
| 8 | **04** Resource limits | — | DoS vectors (UDP session cap, JIT socket limit). Slot wherever convenient. |
| 9 | **06** WS structural fixes | — | Merge pending queues, cap stream-opens. Slot wherever convenient. |

### Tier 3 — Feature completion (13x continued)

| # | Task | Depends on |
|---|------|-----------|
| 10 | **13d** Hints | 13c |
| 11 | **13e** Route announcement | 13c, 07 |
| 12 | **13f** Security posture | 13c, 13d, 13e |
| 13 | **13g** Mode transitions | 13b, 13c, 13d, 07 |

### Tier 4 — Crate extraction

| # | Task | Depends on | What |
|---|------|-----------|------|
| 14 | **extract-protocol** | 07, 11 | `core/src/{client,server,transport,tls}` → `wallhack-protocol` crate |
| 15 | **extract-state** | 10 | `core/src/control/{metrics,peers,routes}` → `wallhack-state` crate |

---

## Architecture Notes

### Why the ordering matters

The 13x series is well-designed — the protocol layer it builds on (`wire` crate) is
solid and the phases are strictly sequenced. The risk is not in the 13x design itself
but in the runtime plumbing underneath it.

**Broadcast channels (07)** — 13a through 13d don't care. 13e (route updates over
channels) and 13g (runtime transitions triggering channel traffic under load) will
care. The broadcast→mpsc migration must land before those phases.

**Transport duplication (11)** — Every 13x phase that touches mode files
(`entry.rs`, `exit.rs`, `relay.rs`) adds more QUIC/WS copy-paste. The debt compounds
with each phase. Doing 11 after 13c but before 13d–13g limits the blast radius.

**Dual TCP path (12)** — Dead code in the hot path. Removing it before 07 and 11
means fewer files to migrate and less confusion about which TCP path is real.

### Crate extraction rationale

`wallhack-core` is a god crate containing client, server, transport, TLS, entry,
exit, control, IPC, and types. The compiler cannot enforce separation of concerns
when everything is `pub(crate)` in one crate. Two extractions fix this:

**`wallhack-protocol`** — Transport setup (client/server/TLS) becomes a separate crate
that physically cannot import `NodeRole` or mode-specific types. This enforces the
boundary between "how we connect" and "what we do once connected". Extract after tasks
07 and 11 clean the code — extracting messy code into a crate just gives you a messy
crate.

**`wallhack-state`** — Shared runtime state (metrics, peer registry, route table)
becomes explicit. Entry, exit, and daemon all reach into these today via `Arc<T>`.
Making it a crate documents the shared contract and prevents control-plane logic from
accumulating in the wrong place. Extract after task 10 so the types being extracted
are clean newtypes, not raw `String` soup.

**Entry/exit as separate crates** — Not yet. The coupling to `control/handler` and
`SharedMetrics` is too tight. After `wallhack-state` exists this becomes feasible but
is not urgent. The mode files in `daemon/src/mode/` already provide sufficient
separation at the orchestration level.

**What does not need extraction:**
- `wire` — already clean, three planes compile independently
- `entry-stack` — already well-isolated, no knowledge of QUIC/WS or roles
- `exit-adapter` — already a separate crate (though tightly coupled to wire types)
- No "roles" or "capabilities" crate needed — that logic belongs in the protocol crate

### What's solid today

- **Wire protocol** — Three planes (data, control, management) properly separated.
  `Handshake` message is forward-looking. Role types in control responses are
  acceptable (operational reality, not implementation leakage).
- **Entry-stack** — Clean smoltcp wrapper. No transport or role knowledge. Good
  sync/async split.
- **Transport trait** — `Transport` trait cleanly abstracts QUIC vs WS. Both
  conform to `BiStream`/`SendStream`/`RecvStream`. WebSocket yamux prefix is ugly
  but contained.
- **Mode dispatch** — `daemon::mode::run()` is a clean routing point. Entry/exit/relay
  have distinct files with distinct responsibilities.

### What needs attention

- **Role leaks into transport setup** — `Client::connect()` and `Server::accept()`
  take `NodeRole`. Protocol framing (`protocol.rs`) dispatches to role-named channels.
  Transport should be role-agnostic; callers decide what channels mean.
- **`ConnectionManager` monolith** — Six concerns in one `select!` loop. Works today
  but a bug in one arm can silently starve another. Extract after 07.
- **Broadcast on data path** — `broadcast::Sender` drops packets when receivers lag.
  This is the scariest runtime correctness issue. Task 07.
- **QUIC/WS duplication in modes** — ~500 lines of copy-paste across entry/exit/relay.
  Adding a transport means touching 3+ files. Task 11.