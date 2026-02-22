# Structured Task Lifecycle and Graceful Shutdown

Replace fire-and-forget `tokio::spawn` calls with `JoinSet`-managed tasks.
This enables graceful shutdown, panic propagation, and bounded concurrency.
Fix the SYN proxy TOCTOU race as a related correctness issue.

## Scope

`crates/wallhack/src/entry/manager.rs`,
`crates/wallhack/src/control/server.rs`,
`crates/wallhack/src/server/quic/mod.rs`,
`crates/wallhack/src/server/ws/mod.rs`

---

## Background

Throughout the codebase, `tokio::spawn` is used without storing the
`JoinHandle`. This means:

1. **Panics vanish.** If a spawned task panics, the error goes to tokio's
   default panic handler (stderr) and is invisible to the parent task.
2. **No graceful shutdown.** When the parent wants to stop, it has no handles
   to await or abort. In-flight TCP sessions, SYN probes, and control requests
   are abandoned rather than drained.
3. **Unbounded concurrency.** Nothing limits how many concurrent tasks exist at
   any time.

---

## Items

### 1. TCP session tasks in `ConnectionManager` — use `JoinSet`

**File:** `entry/manager.rs:154`

```rust
// before
tokio::spawn(async move {
    if let Err(e) = run_tcp_session(stream, transport).await {
        tracing::debug!("TCP session ended: {e}");
    }
    metrics.dec_active_connections();
});

// after
self.tcp_sessions.spawn(async move {
    if let Err(e) = run_tcp_session(stream, transport).await {
        tracing::debug!("TCP session ended: {e}");
    }
    metrics.dec_active_connections();
});
```

Add `tcp_sessions: JoinSet<()>` field to `ConnectionManager`. In the `run`
loop, add a branch to drive the JoinSet:
```rust
Some(result) = self.tcp_sessions.join_next() => {
    if let Err(e) = result {
        tracing::warn!("TCP session task panicked: {e:?}");
    }
}
```

This surfaces task panics and keeps the JoinSet drained.

Optionally: bound concurrency with a `tokio::sync::Semaphore`:
```rust
const MAX_CONCURRENT_TCP_SESSIONS: usize = 1_024;
// acquire a permit before spawning; release it when the task ends
```

### 2. SYN proxy tasks — also use `JoinSet`

**File:** `entry/manager.rs:237`

Same pattern. Add `syn_proxy_tasks: JoinSet<()>` to `ConnectionManager`.
Drive it in the select loop alongside `tcp_sessions`.

**Fix the TOCTOU race here:** Currently, two SYNs for the same port can spawn
two parallel probes. The second probe overwrites the first result. Fix:
before spawning a probe, check if the port is already being probed:

```rust
Some(held) = self.syn_rx.recv() => {
    let port = held.dst_port;
    // Skip if already probing this port
    if self.syn_proxy_state.is_probing(port) {
        tracing::debug!(port, "SYN for already-probing port, re-injecting");
        let _ = self.inject_tx.send(held.packet);
        self.wake_notify.notify_one();
        continue;
    }
    // ... spawn probe task
}
```

### 3. Control server connection handlers

**File:** `control/server.rs:132,198`

The control server spawns one task per connection with no tracking:
```rust
tokio::spawn(async move {
    if let Err(e) = handle_connection(connection, handler).await { ... }
});
```

Replace with a `JoinSet<()>` field on `ControlServer`. Drive it in the accept
loop. This allows the server's `shutdown()` method to actually await in-flight
requests before returning.

### 4. Per-peer accept tasks in server modules

**Files:** `server/quic/mod.rs`, `server/ws/mod.rs`

Same pattern for the accept loops that spawn per-peer tasks. Use a `JoinSet`
so the server can drain connections during graceful shutdown.

### 5. Implement a graceful shutdown path

Once tasks are tracked with `JoinSet`, implement shutdown:
```rust
pub async fn shutdown(mut self) {
    // Signal no new connections
    // Abort or await all pending tasks with a timeout
    tokio::time::timeout(Duration::from_secs(30), async {
        while self.tcp_sessions.join_next().await.is_some() {}
    }).await.ok();
}
```

A 30-second drain window is a reasonable default. This is called by the CLI
on SIGTERM.

---

## Acceptance criteria

- No fire-and-forget `tokio::spawn` for long-lived tasks (short-lived
  classification tasks from task 06 are exempt if bounded by semaphore)
- Task panics in `ConnectionManager`, `ControlServer`, and accept loops are
  logged at `warn!` level
- Two simultaneous SYNs for the same port result in one probe, not two
- `just check` passes
