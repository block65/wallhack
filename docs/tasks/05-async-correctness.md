# Async Correctness Fixes

Fix incorrect async patterns that cause silent hangs, busy loops, unnecessary
allocations, and wrong primitive usage. These are independent of the larger
broadcast→mpsc migration (task 07) and can land first.

## Scope

`crates/exit-adapter/src/sessions/`,
`crates/entry-stack/src/async_stack/`,
`crates/transport/src/ws_adapter.rs`

---

## Items

### 1. Exit-adapter UDP sessions — remove double readiness check and infinite retry

**File:** `crates/exit-adapter/src/sessions/udp.rs`

Both `send` and `recv` call `.writable().await` / `.readable().await` and
then immediately call the *fully async* `.send_to().await` / `.recv().await`.
Those async methods handle readiness internally — the explicit readiness poll
is redundant and causes two back-to-back kernel polls.

Worse, the `WouldBlock` retry arm loops with `yield_now().await` and no
timeout. If the socket never becomes writable (broken peer, flow control
stall), this spins indefinitely.

**Fix:** Follow the pattern already used in `icmp.rs` — use the `try_` variant
gated on a single readiness event:

```rust
// send
loop {
    self.socket.writable().await?;
    match self.socket.try_send_to(buf, dest) {
        Ok(n) => return Ok(SessionStatus::DataIo { size: n }),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue, // spurious wakeup
        Err(e) => return Err(e.into()),
    }
}

// recv — same pattern with try_recv_from
```

Remove the dead `// continue;` comments and the `WouldBlock` arms that fall
through without actually retrying properly.

### 2. ICMP recv — add timeout

**File:** `crates/exit-adapter/src/sessions/icmp.rs:78`

```rust
// NOTE: this will wait forever until data is received
self.recv(recv_buf).await
```

The comment is honest: this hangs indefinitely if the ICMP echo reply is
dropped. Any caller that awaits this without its own timeout hangs the
connection silently.

**Fix:**
```rust
const ICMP_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

tokio::time::timeout(ICMP_REPLY_TIMEOUT, self.recv(recv_buf))
    .await
    .map_err(|_| RuntimeError::Timeout)??
```

Add `Timeout` variant to `RuntimeError`.

### 3. Entry-stack inject receiver — use `parking_lot::Mutex`, not `tokio::sync::Mutex`

**File:** `crates/entry-stack/src/async_stack/mod.rs:402`

```rust
inject_rx: Option<Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>>
```

The call site uses `.try_lock()` in a sync context (inside a `parking_lot`
mutex guard, cannot `.await`). Using `tokio::sync::Mutex` for its `try_lock`
semantics in sync code is the wrong tool. `mpsc::Receiver` is single-owner;
wrapping it in `Arc<Mutex>` at all is a smell.

**Fix:**
- Replace with `Option<parking_lot::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>>`
- Drop the `Arc` — this has a single owner (the `Netstack`)
- The `parking_lot::Mutex::try_lock()` call is sync and correct here

### 4. `TcpListenerAny` — stop cloning HashSet on every wakeup

**File:** `crates/entry-stack/src/async_stack/tcp_listener_any.rs:54`

```rust
let ports = self.ports.lock().clone();  // inside a hot wakeup loop
```

This allocates and populates a new `HashSet<u16>` on every entry-stack wakeup
(which can be hundreds of times per second). The set changes rarely (only when
a new port is registered via JIT or explicit bind).

**Fix:** Replace with a `tokio::sync::watch::Receiver<Arc<HashSet<u16>>>`.
The watch sender updates atomically on port registration; the listener does a
cheap `borrow()` + `Arc::clone()` per wakeup:

```rust
// field
ports: watch::Receiver<Arc<HashSet<u16>>>,

// in the hot loop
let ports = Arc::clone(&*self.ports.borrow());
```

Update the write path (port registration) to call `ports_tx.send(new_arc)`.

### 5. WS write path — eliminate intermediate Vec allocation

**File:** `crates/transport/src/ws_adapter.rs:155`

```rust
// before — allocates Vec, then converts to Bytes
let msg = Message::Binary(buf.to_vec().into());

// after — single allocation directly into Bytes
let msg = Message::Binary(Bytes::copy_from_slice(buf));
```

This is on the write hot path for every tunnel packet. Minor but measurable
at high packet rates.

### 6. WS read buffer — enforce maximum message size

**File:** `crates/transport/src/ws_adapter.rs:103-104`

If the WebSocket peer sends a single large message (e.g. 64 MiB), the entire
payload is held in `read_buf` until the consumer drains it. There is no
`max_message_size` configured at the tungstenite layer.

**Fix:** Pass `WebSocketConfig` when constructing the WebSocket connection:
```rust
const MAX_WS_MESSAGE_SIZE: usize = 64 * 1024 + 512; // MTU + overhead

let config = WebSocketConfig {
    max_message_size: Some(MAX_WS_MESSAGE_SIZE),
    max_frame_size: Some(MAX_WS_MESSAGE_SIZE),
    ..Default::default()
};
```

Anything larger is a protocol violation that should be rejected at the
tungstenite layer rather than buffered.

## Acceptance criteria

- `just check` passes
- No `tokio::sync::Mutex` wrapping an mpsc `Receiver` anywhere
- UDP session send/recv use `try_send_to`/`try_recv_from` with single
  readiness poll
- ICMP recv returns `Err(RuntimeError::Timeout)` after 5 s with no reply
- Writing 1000 small packets via `WsByteStream` allocates one `Bytes` per
  packet, not two (`Vec` + `Bytes`)
