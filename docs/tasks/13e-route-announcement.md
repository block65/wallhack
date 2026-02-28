# Phase 13e: Route Announcement

Extends the handshake to include network reachability information.
Each node advertises the routes it can reach, and the entry node automatically
installs those routes into its TUN routing table. No manual `ip route`
configuration is required on the operator's side.

**Design spec:** `docs/tasks/13-zero-config-and-friends.md`
**Depends on:** Phase 13a (handshake — routes field already defined),
Phase 13c (auto-negotiation — modes must be resolved before routes are acted on)

---

## Scope

`crates/wire/proto/data.proto`,
`crates/wire/proto/control.proto`,
`crates/core/src/control/routes.rs`,
`crates/core/src/entry/` (TUN routing table management),
`crates/daemon/src/mode/entry.rs`,
`crates/daemon/src/mode/relay.rs`,
`crates/cli/src/daemon_cli.rs`

---

## Items

### 1. Route announcement in handshake

The `routes` field (repeated string, CIDR notation) is already defined in
the `Handshake` message (Phase 13a, field 7). On connection and role
resolution, each node populates this field with the networks reachable from
its local interfaces, subject to the filtering policy below.

The entry node receives these announcements, validates them (see item 7),
and adds the accepted routes to the TUN, pointing them toward the peer that
announced them.

**Route filtering policy (what to announce):**

Not all local interfaces should be announced. A typical host has loopback,
Docker bridges, VPN tunnels, and other interfaces that are either nonsensical
to route through the tunnel or would leak unnecessary information. The
default filtering policy:

- **Skip loopback** — `127.0.0.0/8`, `::1/128`. Never useful.
- **Skip link-local** — `169.254.0.0/16`, `fe80::/10`. Not routable.
- **Skip the tunnel interface itself** — the TUN/utun interface created by
  wallhack. Announcing the tunnel's own subnet would create a routing loop.
- **Announce everything else** — including RFC1918 ranges (`10.0.0.0/8`,
  `172.16.0.0/12`, `192.168.0.0/16`). Private ranges are the primary use
  case for pivoting.

The operator can suppress all announcements with `--no-announce-routes` (item
6). A future enhancement could add `--announce-routes <cidr>[,<cidr>]` for
explicit control over which networks to advertise, but that is out of scope
for this phase.

### 2. Define `RouteUpdate` and `RouteWithdraw` messages

**File:** `crates/wire/proto/control.proto`

```protobuf
message RouteUpdate {
  repeated string routes = 1;     // CIDR notation, newly reachable networks
  string origin_peer = 2;         // Stable name of the peer that originally announced
}

message RouteWithdraw {
  repeated string routes = 1;     // CIDR notation, no longer reachable
  string origin_peer = 2;         // Must match the origin_peer in the original RouteUpdate
}
```

**`origin_peer` identity:** This is the `name` field from the peer's
`Handshake` message — the stable identifier provided via `--name` or
auto-generated at startup. It must be stable across the lifetime of a
connection. On reconnection, the full handshake repeats and all
routes from the previous session are cleared regardless of name, so name
stability across reconnections is not required (but is desirable for
operator clarity). If the peer's name changes across reconnections, the
previous session's routes are already withdrawn by the reconnection cleanup.

Add these as variants in `ControlMessage`. Route updates are informational —
they do not require acknowledgement.

### 3. Relay route forwarding

A relay receives route announcements from its peer and forwards them toward
entry as a `RouteUpdate`, preserving the originating `origin_peer` identity
for withdrawal purposes. The relay does not merge or aggregate routes — each
announcement is forwarded independently.

As the chain grows (entry → relay → relay → exit), routes propagate hop by
hop toward the entry node. For a chain with N relays, a single exit's route
announcement produces N `RouteUpdate` messages (one per relay hop). This is
O(N) in the chain length, which is a known property — at wallhack's scale
(chains of 2-5 nodes typically) this is negligible. The linear fan-out is an
inherent property of the chain topology and not worth optimising.

### 4. Dynamic route lifecycle

Routes follow the same lifecycle as modes:

- **On connection** — initial routes announced in `Handshake` handshake
- **On role change** — `RouteUpdate` sent alongside any role transition with
  changed routes
- **On transition to indeterminate** — `RouteWithdraw` sent; entry removes
  affected routes from the TUN routing table immediately
- **On reconnection** — full `Handshake` handshake repeats, routes
  re-announced from scratch, no prior state assumed

### 5. TUN routing table as a live map

The entry node's TUN routing table becomes a real-time map of everything
reachable through the current chain. As the engagement progresses and the
chain grows, the table grows with it. Removing a node from the chain removes
its routes.

Integrate with the existing `SharedRouteTable` in `control/routes.rs`. Routes
added via auto-announcement should be tagged as auto-managed so they can be
distinguished from manually-added routes and withdrawn cleanly.

### 6. Suppression flags

**File:** `crates/cli/src/daemon_cli.rs`

| Flag | Effect |
|---|---|
| `--no-announce-routes` | This node does not advertise its local routes to peers |
| `--no-accept-routes` | Entry node does not auto-install announced routes into TUN |

`--no-announce-routes` is useful when the tunnel is desired but leaking
internal network topology in the handshake is not.

`--no-accept-routes` is useful when the operator wants to manage the
entry-side routing table manually.

### 7. Route validation on acceptance

The entry node must validate announced routes before installing them. A
malicious or misconfigured peer could announce `0.0.0.0/0`, which would
install a default route through the tunnel and capture all traffic on the
entry host — potentially breaking connectivity entirely.

**Default validation rules:**

- **Reject default routes** — `0.0.0.0/0` and `::/0` are never accepted
  automatically. If the operator wants a full tunnel, they can add the
  default route manually via `route add`.
- **Reject loopback and link-local** — same ranges excluded from
  announcement (item 1). These should never appear in a route update.
- **Reject the entry node's own local subnets** — if the entry host is on
  `192.168.1.0/24` and a peer announces `192.168.1.0/24`, installing that
  route would hijack the entry's own LAN traffic. Reject routes that overlap
  with the entry node's directly-connected networks.
- **Only touch routes on our own interfaces** — wallhack only ever adds or
  removes routes that point through TUN interfaces it created. It never
  modifies, replaces, or removes routes belonging to other interfaces. If
  a route add fails (prefix already exists, permission denied, etc.), skip
  it and log at WARN. Auto-routing is best-effort.
- **Accept everything else** — including RFC1918 ranges, which are the
  primary pivoting use case.

**Optional granular control (future):** An `--accept-routes <cidr>` flag
could allow the operator to whitelist specific prefixes, providing a middle
ground between `--no-accept-routes` (nothing) and the default (everything
that passes validation). Out of scope for this phase but noted as a natural
extension.

---

## Tests

- **Route announcement round-trip** — exit announces routes in capability
  handshake, entry receives them and installs in TUN routing table. Verify
  routes are present.
- **Route withdrawal on disconnect** — exit disconnects, entry receives
  `RouteWithdraw` (or detects disconnect), removes routes from TUN. Verify
  routes are gone.
- **Route withdrawal on indeterminate** — exit transitions to indeterminate,
  sends `RouteWithdraw`. Entry removes routes immediately.
- **Relay forwarding** — exit announces routes, relay forwards `RouteUpdate`
  upstream to entry. Entry installs routes pointing through the relay. Verify
  origin peer identity is preserved.
- **Per-peer withdrawal** — entry installs routes from two different peers
  (e.g. via two separate relay chains). One peer disconnects. Entry withdraws
  only that peer's routes; the other peer's routes are unaffected. Tests the
  `origin_peer` identity matching.
- **Suppression: `--no-announce-routes`** — node started with this flag sends
  empty routes in handshake. Verify no routes are advertised.
- **Suppression: `--no-accept-routes`** — entry node started with this flag
  ignores announced routes. Verify TUN routing table is not modified.
- **Auto-managed vs manual routes** — manually added routes are not affected
  by auto-route withdrawal. Auto-announced routes are not affected by manual
  `route del`.
- **Reconnection clears and re-announces** — on reconnection, all
  auto-managed routes from the previous session are cleared before the new
  handshake routes are installed.
- **Route filtering: loopback excluded** — node with loopback interface does
  not include `127.0.0.0/8` in announced routes.
- **Route filtering: link-local excluded** — `169.254.0.0/16` and
  `fe80::/10` excluded from announcements.
- **Route filtering: tunnel interface excluded** — the wallhack TUN/utun
  subnet is not announced.
- **Route validation: default route rejected** — peer announces `0.0.0.0/0`.
  Entry does not install it. Warning logged.
- **Route validation: loopback rejected** — peer announces `127.0.0.0/8`.
  Entry rejects it.
- **Route validation: own subnet rejected** — entry is on `192.168.1.0/24`.
  Peer announces `192.168.1.0/24`. Entry rejects it (would hijack local LAN).
- **Route collision skipped** — system routing table already has a route
  for `10.0.50.0/24` on another interface. Peer announces `10.0.50.0/24`.
  Route add fails, entry skips it and logs warning. No crash, no retry.
  The existing route on the other interface is never modified.
- **Cleanup only touches own routes** — on disconnect/shutdown, only routes
  pointing through wallhack's TUN interfaces are removed. Routes on other
  interfaces are never touched.
- **Route validation: normal RFC1918 accepted** — peer announces
  `10.0.50.0/24`. Entry accepts and installs it.

---

## Acceptance Criteria

- Entry node's TUN routing table is automatically populated from peer
  route announcements
- Routes propagate along the chain through relays
- Routes are withdrawn cleanly on disconnect or transition to indeterminate
- `--no-announce-routes` suppresses route advertisement
- `--no-accept-routes` suppresses route installation
- `wallhack route` output distinguishes auto-managed from manual routes
- Route filtering excludes loopback, link-local, and tunnel interfaces
- Route validation rejects default routes, loopback, and entry's own subnets
- Wallhack only adds/removes routes on TUN interfaces it owns, never
  touches routes belonging to other interfaces
- All tests pass
