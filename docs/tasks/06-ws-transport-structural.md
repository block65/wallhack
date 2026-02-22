# WebSocket Transport Structural Fixes

Fix two structural issues in the WebSocket transport driver that are separate
from the general async fixes in task 05. Both require changes to the `Driver`
struct internals and the public `Transport` trait surface.

## Scope

`crates/transport/src/ws.rs`

---

## Background

The WebSocket transport uses yamux for stream multiplexing. Yamux has no
concept of unidirectional vs bidirectional streams — the distinction is
enforced by writing a single prefix byte when a stream is opened (`0x00` for
data/uni, `0x01` for control/bi). The current `Driver` implementation pretends
to maintain two separate queues for uni and bi stream openings, but both queues
call the same yamux primitive (`poll_new_outbound`). The enforcement is a
naming convention with no runtime guarantee.

---

## Items

### 1. Enforce stream kind at open time — merge the two pending queues

**Problem:**
```rust
// both queues call poll_new_outbound — distinction is a fiction
pending_open_uni: VecDeque<oneshot::Sender<Result<YamuxStream, TransportError>>>,
pending_open_bi:  VecDeque<oneshot::Sender<Result<YamuxStream, TransportError>>>,
```

A caller that enqueues in `pending_open_uni` can treat the returned stream as
bidirectional — the driver cannot stop them. The prefix byte that distinguishes
stream types is written by the *caller* rather than by the driver at open time.

**Fix:** Merge into one queue carrying the stream kind:
```rust
enum StreamKind { Data, Control }

pending_open: VecDeque<(StreamKind, oneshot::Sender<Result<YamuxStream, TransportError>>)>,
```

When yamux opens a new outbound stream, the driver writes the kind byte
*immediately* before returning the stream to the caller:
```rust
stream.write_all(&[kind.prefix_byte()]).await?;
oneshot_tx.send(Ok(stream)).ok();
```

This moves enforcement from convention to code.

### 2. Cap pending stream-open queues

**Problem:** `pending_open_uni` and `pending_open_bi` (or the merged
`pending_open`) grow without bound when the yamux connection is stalled (peer
slow, flow-control window exhausted). Callers block waiting for `open_bi` or
`open_uni` to complete; the driver keeps accepting requests and growing the
queue.

**Fix:** Cap at a high-watermark and return an error when full:
```rust
const MAX_PENDING_STREAM_OPENS: usize = 256;

if self.pending_open.len() >= MAX_PENDING_STREAM_OPENS {
    oneshot_tx.send(Err(TransportError::overloaded())).ok();
    return;
}
self.pending_open.push_back((kind, oneshot_tx));
```

Add `TransportError::overloaded()` constructor if it doesn't exist.

### 3. Cap concurrent inbound-stream-classification tasks

**Problem:** The driver spawns a `tokio::spawn` per incoming yamux stream to
read the prefix byte. There is no limit on concurrent tasks. A stream-flood
(many rapid `open_bi` calls from the peer) creates unbounded tasks, each
blocked trying to send to a bounded channel.

**Fix:** Use a `tokio::sync::Semaphore` with `MAX_CONCURRENT_CLASSIFICATIONS`
permits (e.g. 128). The spawned task acquires a permit; the driver only spawns
if a permit is available, otherwise closes the stream:
```rust
const MAX_CONCURRENT_CLASSIFICATIONS: usize = 128;
// field: classification_semaphore: Arc<Semaphore>

let permit = match self.classification_semaphore.clone().try_acquire_owned() {
    Ok(p) => p,
    Err(_) => {
        tracing::warn!("stream classification limit reached, closing stream");
        return; // stream dropped, yamux will see a close
    }
};
tokio::spawn(async move {
    let _permit = permit; // hold until done
    // ... read prefix, send to channel
});
```

## Notes

- Item 1 touches the public `Transport` trait API (the `open_uni`/`open_bi`
  method signatures do not change, but the internal routing does). Check all
  call sites in `cli/` and `wallhack/` still compile.
- Items 2 and 3 are independent of item 1 and can be done first if easier.

## Acceptance criteria

- `just check` passes
- The prefix byte is written by the driver at stream-open time, not by the
  caller
- `pending_open` queue length is bounded by `MAX_PENDING_STREAM_OPENS`
- Number of concurrent classification tasks is bounded by
  `MAX_CONCURRENT_CLASSIFICATIONS`
- Existing WS integration test still passes
