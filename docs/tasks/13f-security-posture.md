# Phase 13f: Security Posture

When any authentication flag is provided, wallhack automatically adopts a
hardened configuration that suppresses auto-negotiation and auto-routing. The
rationale is that an operator who cares enough about authentication almost
certainly also cares about the OPSEC implications of automatic behaviour. The
hardened defaults bundle both concerns so neither has to be remembered
separately.

**Design spec:** `docs/tasks/13-zero-config-and-friends.md`
**Depends on:** Phase 13c (auto-negotiation), Phase 13d (hints — `--fixed-role`),
Phase 13e (route announcement)

---

## Scope

`crates/cli/src/daemon_cli.rs`,
`crates/daemon/src/daemon_config.rs`,
`crates/daemon/src/mode/mod.rs`

---

## Terminology

- **Security posture** — the only correct use of "posture" in the codebase.
  Refers to the authentication and behavioural hardening configuration. Not
  to be confused with connectivity (listener/connector/both).
- **Default posture** — TLS encryption with no certificate verification.
  Encrypted against passive interception but not authenticated.
- **Secure posture** — triggered by any auth flag. Connections are verified
  and auto-negotiation is suppressed.

---

## Items

### 1. Auth flags trigger secure posture

When any of the following auth flags is present, secure posture activates
as a bundle:

| Flag | Status | Effect |
|---|---|---|
| `--psk <key>` | Exists | Pre-shared key authentication |
| `--accept-fingerprint <fp>` | Exists | Pin expected peer certificate fingerprint |
| `--ca <path>` | Exists | Mutual TLS — verify peer certificates against this CA bundle |

All three flags already exist in the codebase. The secure posture behaviour
(bundling hardened defaults with any auth flag) is the new part — the auth
mechanisms themselves are pre-existing.

**mTLS mechanics.** mTLS is configured through the existing `--cert`,
`--key`, and `--ca` flags. `--cert` and `--key` provide the node's own
certificate and private key. `--ca` on the server side enables client
certificate verification against the specified CA bundle. The `--ca` flag
is what triggers secure posture for mTLS — it is the flag that says
"verify the peer." `--cert` and `--key` alone do not trigger secure
posture (they configure the node's own identity, not peer verification).

**WebSocket client mTLS limitation.** The WebSocket client does not
currently support presenting client certificates (the mTLS config is not
wired through). If `--ca` is provided and the transport is WebSocket,
this is a startup error — not a silent skip. WebSocket client mTLS
support is a separate task.

**`--accept-fingerprint` semantics.** This flag pins the expected peer
certificate fingerprint. The format is `sha256:<hex>` (the `sha256:`
prefix is required). This is not TOFU — the fingerprint must be known
ahead of time and provided at startup. The existing implementation in
`crates/core/src/tls/verifiers.rs` handles the verification. Phase 13f
does not change the fingerprint mechanism — it only makes the flag trigger
hardened defaults alongside its existing verification behaviour.

When secure posture is active:
- The specified verification is enforced — connections that fail verification
  are rejected
- Auto-negotiation is suppressed — `--fixed-role` becomes the default
  (role must be provided explicitly or via deprecated subcommand)
- Auto-routing is suppressed — routes are not announced or accepted
  automatically

### 2. Re-enabling zero-config under auth

When secure posture is active, the operator can explicitly re-enable
zero-config behaviour:

| Flag | Effect |
|---|---|
| `--auto-negotiate` | Re-enables role negotiation only, routes still manual |
| `--auto-routes` | Re-enables route announcement and injection only, role still fixed |
| `--zero-config` | Re-enables both — shorthand for `--auto-negotiate --auto-routes` |

```
wallhack --connect host:6565 --psk abc123 --zero-config
```

`--zero-config` is an explicit acknowledgement that the OPSEC tradeoffs of
both systems are understood and accepted. Its presence makes that decision
visible and auditable.

### 3. Configuration resolution

The configuration is derived from the flag combination. Auth enforcement and
hardened defaults are separate concerns:

```
has_auth_flag := psk || ca || accept_fingerprint

# Auth is ALWAYS enforced when auth flags are present.
# --zero-config does not weaken authentication.
auth_required := has_auth_flag

# Hardened defaults suppress auto-behaviour.
# --zero-config, --auto-negotiate, and --auto-routes override selectively.
hardened_defaults := has_auth_flag && !zero_config

if hardened_defaults:
    negotiate := --auto-negotiate was provided
    routes := --auto-routes was provided
else:
    negotiate := true
    routes := true
```

**Key invariant:** `--zero-config` re-enables auto-negotiation and
auto-routing. It does NOT disable authentication. Auth verification is always
enforced when auth flags are present, regardless of any other flag.

When `--zero-config` is present alongside auth flags, both `negotiate` and
`routes` are `true`, but `auth_required` remains `true`. The node negotiates
freely but still rejects unauthenticated connections.

### 4. Interaction with hints (Phase 13d)

When hardened defaults are active (auth flag present, no `--zero-config` or
`--auto-negotiate`), soft hints (`--prefer`) are incompatible — they require
auto-negotiation to function, and auto-negotiation is suppressed.

**Resolution:** if `--prefer <role>` is provided alongside an auth flag
without `--auto-negotiate` or `--zero-config`, this is a **startup error**:
`"--prefer requires --auto-negotiate or --zero-config under secure posture"`.
No silent promotion, no silent ignore.

`--fixed-role` and `--exclude-role` work under any posture — they are
explicit constraints, not soft negotiation hints.

This is specified in both Phase 13d (item 6) and here. The rule is simple:
soft hints need negotiation; hardened defaults suppress negotiation; providing
both without an explicit override is a configuration error.

### 5. OPSEC documentation

Update the CLI `--help` output and any user-facing documentation to make the
security posture behaviour clear. The key message:

- Auth flags harden the node automatically — no additional flags needed for
  safe deployment on a target network
- `--zero-config` is the explicit opt-in to automatic behaviour under auth
- `--fixed-role` is always available regardless of posture

---

## Tests

- **Auth triggers secure posture** — provide `--psk`, verify auto-negotiation
  is suppressed and auto-routing is suppressed.
- **Each auth flag individually** — `--psk`, `--ca`,
  `--accept-fingerprint` each independently trigger hardened defaults.
- **No auth = default posture** — no auth flags, auto-negotiation and
  auto-routing both active.
- **`--zero-config` re-enables both** — `--psk` + `--zero-config`, verify
  auto-negotiation and auto-routing are both active.
- **`--auto-negotiate` granular** — `--psk` + `--auto-negotiate`, verify
  negotiation is active but routes are suppressed.
- **`--auto-routes` granular** — `--psk` + `--auto-routes`, verify routes are
  active but role is fixed.
- **`--zero-config` without auth** — no auth flags + `--zero-config`. Flag is
  accepted but has no effect (already the default).
- **Role required under secure posture** — `--psk` without `--fixed-role`,
  subcommand, or `--auto-negotiate`. Startup error:
  `"--psk requires --fixed-role <role>, a subcommand, or --auto-negotiate"`.
  No silent defaults — the operator must be explicit.
- **Auth always enforced** — `--psk abc123 --zero-config`, connect without
  PSK. Verify connection is rejected. Auth is never weakened by
  `--zero-config`.
- **`--prefer` + auth = startup error** — `--psk abc123 --prefer entry`
  without `--auto-negotiate`. Startup error, not silent ignore.
- **`--prefer` + auth + `--auto-negotiate` = ok** — `--psk abc123
  --auto-negotiate --prefer entry`. Starts successfully, negotiation active,
  auth enforced.
- **`--fixed-role` + auth = ok** — `--psk abc123 --fixed-role exit`. Starts
  successfully, no `--auto-negotiate` needed.

---

## Acceptance Criteria

- Any auth flag automatically suppresses auto-negotiation and auto-routing
  (hardened defaults)
- `--zero-config` re-enables both alongside auth, without weakening auth
- `--auto-negotiate` and `--auto-routes` provide granular control
- Auth verification is always enforced when auth flags are present,
  regardless of `--zero-config`
- `--prefer` + auth flag without `--auto-negotiate` is a startup error
- `--fixed-role` and `--exclude-role` work under any posture
- Auth flag without explicit role or `--auto-negotiate` is a startup error
- The configuration resolution logic is a deterministic function with
  complete test coverage
- CLI `--help` documents the security posture behaviour
- All tests pass
