# Trivial Cleanup Sprint

Batch of low-risk, high-confidence fixes. No behaviour changes, no new
features. Every item should compile and pass `just check` independently.

## Scope

Across all crates — see individual items below.

## Why

Dead code, silent-drop patterns, and stale comments accumulate reviewer
fatigue and mask real bugs. These are the easiest PRs to land and the most
embarrassing to leave in a public repo.

---

## Items

### 1. Delete all commented-out module/code blocks

| File | Content |
|------|---------|
| `crates/wallhack/src/lib.rs:9` | `// mod channel;` |
| `crates/wallhack/src/tls/mod.rs:1` | `// pub mod self_signed;` |
| `crates/wallhack/src/exit/mod.rs:1` | `// pub mod adapter;` |
| `crates/exit-adapter/src/lib.rs:4-5` | Commented feature gate block |
| `crates/protobuf/src/v2.rs:207-241` | Commented `From`/`TryFrom` impls |
| `crates/wallhack/src/test_helpers.rs:51-62` | `/* pub async fn connect_test_client */` |
| `crates/cli/src/session.rs:175-180` | Commented-out test block |
| `crates/exit-adapter/src/adapter.rs:35-36` | Commented error variant |
| `crates/wallhack/src/server/create.rs:38-41` | Commented server config block |

Delete every line. No replacements. Each is in git history if ever needed.

### 2. Delete dead prototype code in `cli/src/session.rs`

- `trait _StatsForNerds` and its `impl` on `NetServer`
- The `_stop` method on `NetServer`
- The test that asserts the stub string `"Server is not listening"`

All are underscore-prefixed scaffolding that was never wired up.

### 3. Delete `NodeApi` trait

`crates/wallhack/src/api/node_api.rs` declares `pub trait NodeApi` with zero
implementors anywhere in the workspace. Delete the trait and the file.

### 4. Delete `TokioAsyncReadWrite` trait

`crates/transport/src/ws.rs` declares `pub trait TokioAsyncReadWrite: AsyncRead + AsyncWrite + Send + Unpin`
with zero implementors. Delete the declaration.

### 5. Fix silent TCP session error drop

`crates/wallhack/src/entry/manager.rs:154`:
```rust
// before
let _ = run_tcp_session(stream, transport).await;

// after
if let Err(e) = run_tcp_session(stream, transport).await {
    tracing::debug!("TCP session ended: {e}");
}
```

### 6. Log Hello send failure in bridge

`crates/wallhack/src/transport/bridge.rs`:
```rust
// before
let _ = tx.send(hello);

// after
if tx.send(hello).is_err() {
    tracing::debug!("hello receiver dropped before auth could complete");
}
```

### 7. Log control response send failure in bridge

`crates/wallhack/src/transport/bridge.rs`:
```rust
// before
let _ = tx.send(resp).await;

// after
if tx.send(resp).await.is_err() {
    tracing::debug!("control response receiver dropped — handler exited?");
}
```

### 8. Log WS stream classifier silent drops

`crates/transport/src/ws.rs:228-233` — three silent failure paths in the
spawned stream-prefix task:
- Timeout → `tracing::warn!("stream prefix read timed out")`
- Unknown prefix byte → `tracing::warn!(prefix = prefix[0], "unknown stream type, dropping")`
- Channel send failure → `tracing::warn!("stream dispatcher full, dropping stream")`
- Replace bare `_ => {}` arm with the warn.

### 9. Log JIT bind failures in netstack

`crates/netstack/src/async_stack/mod.rs`:
```rust
// before
let _ = jit_bind_port(...);

// after
if let Err(e) = jit_bind_port(...) {
    tracing::warn!(port = dst_port, %e, "JIT bind failed");
}
```

### 10. Log SYN re-inject send failures

`crates/wallhack/src/entry/manager.rs:249,254`:
```rust
// before
let _ = inject_tx.send(held.packet);

// after
if inject_tx.send(held.packet).is_err() {
    tracing::warn!("SYN inject channel closed — poll loop dead?");
}
```

### 11. Fix timer drift in UDP cleanup loop

`crates/wallhack/src/entry/manager.rs:259`:
```rust
// before
() = tokio::time::sleep(Duration::from_secs(5)) => { ... }

// after (declare once before the loop)
let mut cleanup_interval = tokio::time::interval(Duration::from_secs(5));
cleanup_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
// in select:
_ = cleanup_interval.tick() => { ... }
```

### 12. Remove spurious clippy allow

`crates/wallhack/src/entry/manager.rs:293`:
```rust
// delete this — SocketAddr::port() returns u16, no cast occurs
#[allow(clippy::cast_possible_truncation)]
```

### 13. Name the broadcast channel capacity constant

`crates/wallhack/src/client/quic/mod.rs:163-164` and the matching site in
`server/quic/mod.rs`:
```rust
/// Capacity of the instructions/responses broadcast channels.
/// Each slot holds one protobuf message; 1024 is enough for a 100ms burst
/// at 10k pps before lagging. See also: TODO broadcast→mpsc migration.
const BROADCAST_CHANNEL_CAPACITY: usize = 1_024;
```
Replace the magic `65536` with the named constant. Do not change the value yet
(that is task 07); just give it a name and a comment.

### 14. Update ICMP ident comment for cross-platform accuracy

`crates/exit-adapter/src/sessions/icmp.rs:44,57`:
```rust
// before
ident: 0x0, // ident is ignored and assigned by the OS instead

// after
// Linux rewrites the ICMP identifier on SOCK_RAW sockets (see ip(7)).
// macOS/BSD do NOT, so replies cannot be correlated by ident on those
// platforms. This code is #[cfg(unix)] but is Linux-only in practice.
ident: 0x0,
```

### 15. Audit and fix `#[allow(clippy::...)]` call sites

Grep the entire workspace for `#[allow(clippy::` and for each occurrence:
- Confirm the suppression is still needed (run clippy, remove the attribute,
  see if the warning fires)
- If needed: add a `// Reason: <why>` comment on the same line
- If not needed: delete the attribute

Pay particular attention to any `#[allow(clippy::too_many_lines)]` and
`#[allow(clippy::too_many_arguments)]` — these are usually suppressing a
symptom of a structural problem tracked elsewhere.

## Out of scope

Behaviour changes, new features, API changes.

## Acceptance criteria

- `just check` passes
- `git diff --stat` shows only deletions and comment/attribute changes for
  items 1–14
- All `#[allow(clippy::...)]` sites have a `// Reason:` comment or are deleted
