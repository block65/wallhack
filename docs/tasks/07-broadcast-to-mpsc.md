# Replace Broadcast Channels with mpsc on the Data Path

`tokio::sync::broadcast` is the wrong primitive for the
instructions/responses data channel. Replace it with `mpsc` to get
backpressure, eliminate silent packet loss, and cut memory usage.

## Scope

`crates/wallhack/src/entry/manager.rs`,
`crates/wallhack/src/exit/orchestrator.rs`,
`crates/wallhack/src/transport/bridge.rs`,
`crates/wallhack/src/server/quic/mod.rs`,
`crates/wallhack/src/server/ws/mod.rs`,
`crates/wallhack/src/client/quic/mod.rs`,
`crates/wallhack/src/client/ws/mod.rs`

---

## Why

Broadcast channels are designed for pub/sub fan-out where **all** receivers
need **all** messages. The instructions/responses data path is point-to-point
(one sender, one receiver per connection). Using broadcast for this gives:

1. **No backpressure.** When the receiver is slow, the sender doesn't block.
   Messages buffer up to capacity, then the receiver *lags* and silently loses
   messages permanently (`RecvError::Lagged`). The current capacity is 65 536
   (or 1 024 after task 01). Either way, under load UDP responses are dropped
   without the caller knowing.

2. **Clone overhead.** Every `recv()` on a broadcast channel clones the
   message. For `ExitNodeResponse` carrying `Bytes` payloads this means atomic
   refcount increments on every packet.

3. **Memory waste.** A broadcast ring buffer holds capacity × sizeof(Message)
   in memory per channel, allocated at creation, per connection. At 100
   concurrent connections × two channels × 65 536 capacity ≈ GiBs of reserved
   memory (depending on message size).

4. **Wrong semantics.** Broadcast guarantees delivery to all current
   receivers at the time of send. There is only one receiver per channel, so
   this guarantee is wasted and the lag mechanism fires incorrectly.

`mpsc::channel` with a bounded capacity is correct: backpressure propagates
naturally, no cloning occurs at the channel level, and the capacity reservation
matches actual queue depth rather than a worst-case ring buffer.

---

## Plan

### 1. Change channel types at creation sites

In `server/quic/mod.rs`, `server/ws/mod.rs`, `client/quic/mod.rs`,
`client/ws/mod.rs`:

```rust
// before
let (instructions_tx, _) = broadcast::channel::<EntryNodeInstruction>(BROADCAST_CHANNEL_CAPACITY);
let (responses_tx, _)    = broadcast::channel::<ExitNodeResponse>(BROADCAST_CHANNEL_CAPACITY);

// after — bounded mpsc
const INSTRUCTIONS_CHANNEL_DEPTH: usize = 1_024;
const RESPONSES_CHANNEL_DEPTH: usize    = 1_024;

let (instructions_tx, instructions_rx) = mpsc::channel::<EntryNodeInstruction>(INSTRUCTIONS_CHANNEL_DEPTH);
let (responses_tx, responses_rx)       = mpsc::channel::<ExitNodeResponse>(RESPONSES_CHANNEL_DEPTH);
```

### 2. Update `ConnectionManager` to hold a mpsc `Receiver`

`entry/manager.rs`:
```rust
// before
responses_rx: broadcast::Receiver<ExitNodeResponse>,

// after
responses_rx: mpsc::Receiver<ExitNodeResponse>,
```

In the select loop:
```rust
// before
result = self.responses_rx.recv() => {
    match result {
        Ok(response) => ...
        Err(RecvError::Lagged(n)) => { warn!(...) }
        Err(RecvError::Closed) => return Ok(()),
    }
}

// after
result = self.responses_rx.recv() => {
    match result {
        Some(response) => self.handle_exit_response(&mut udp, response),
        None => return Ok(()),  // sender dropped, connection dead
    }
}
```

### 3. Update `Orchestrator` to hold a mpsc `Sender`

`exit/orchestrator.rs`:
```rust
// before
responses_tx: broadcast::Sender<ExitNodeResponse>,

// after
responses_tx: mpsc::Sender<ExitNodeResponse>,
```

Replace `responses_tx.send(response)` with `responses_tx.send(response).await`.
Handle `SendError` (receiver dropped) as a graceful shutdown signal.

### 4. Update `bridge.rs` to route mpsc senders/receivers

The bridge currently takes `broadcast::Sender/Receiver` parameters. Update the
type signatures. The semantics remain the same — the bridge is wiring channels
through, not broadcasting.

### 5. Add a dropped-packet metric

When `responses_tx.send(...).await` returns `Err` (receiver gone) or when the
channel is full (on `try_send`), increment a metric:
```rust
self.metrics.inc_responses_dropped(1);
```

Expose this via the stats API so operators can observe backpressure.

### 6. Remove the `Lagged` handling code

After the migration, delete all `RecvError::Lagged` match arms — they no
longer exist on mpsc. Also remove the capacity constant from task 01 (it's
replaced by the per-channel `DEPTH` constants here).

---

## Notes

- The `instructions` channel is entry→exit (low rate: one per UDP packet or
  TCP connection). The `responses` channel is exit→entry (high rate: one per
  UDP reply). Tune `RESPONSES_CHANNEL_DEPTH` separately and larger.
- If there are call sites that subscribe to the broadcast channel from multiple
  tasks, those need to be found and the subscription points consolidated into
  a single mpsc receiver before the switch. Do a grep for `.subscribe()` before
  starting.
- The control channel (ControlRequest/ControlResponse) is a different path and
  already uses mpsc in some places — do not mix these up.

## Acceptance criteria

- No `tokio::sync::broadcast` anywhere on the data path
- `RecvError::Lagged` does not exist in the codebase (it's a broadcast-only type)
- Channel capacity is expressed as named constants with doc comments
- A `responses_dropped` metric exists and is incremented when the channel is
  full or the receiver is gone
- `just check` passes
- Memory budget test (`tests/memory_budget.rs`) still passes
