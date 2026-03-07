# Task: Fix tcp_downstream throughput asymmetry

## Status: PLANNED

## Problem

`tcp_downstream` (entry receives from exit) is ~188 Mbps on QUIC vs ~987 Mbps for
`tcp_upstream` (entry sends to exit). QUIC is disproportionately affected — WebSocket
shows ~460 Mbps — because QUIC flow control is application-coupled: the receive window
only advances as fast as the application consumes data, so a slow consumer directly
throttles the sender.

The bottleneck is **not** `SyscallExitAdapter` (TODO.md was wrong — that's Path B dead
code for TCP benchmarks). The bottleneck is the **entry-stack smoltcp TX path**.

## Root cause

`poll_write` in `crates/entry-stack/src/async_stack/tcp_stream.rs` (line ~160):

```rust
Ok(n) => {
    drop(inner);
    self.shared.notify.notify_one();  // deferred flush
    Poll::Ready(Ok(n))
}
```

Flow for tcp_downstream (exit sends → entry receives → TUN):
1. QUIC layer receives data from exit
2. `copy_bidirectional` calls `poll_write` on the smoltcp `TcpStream`
3. `poll_write` acquires lock, calls `socket.send_slice` → data in smoltcp TX buffer
4. Drops lock, calls `notify_one`
5. **Tokio scheduler round-trip** — poll loop wakes on next scheduler tick
6. Poll loop acquires lock, calls `inner.poll(now)` → smoltcp emits packets → TUN write

Step 5 is the bottleneck: one scheduler round-trip per write burst before data leaves
the machine. For tx_upstream (entry sends), the RX path uses epoll which fires
immediately — no scheduler hop.

## Benchmark data confirming the fix target

| Scenario       | QUIC      | WebSocket |
|----------------|-----------|-----------|
| tcp_upstream   | 987 Mbps  | 991 Mbps  |
| tcp_downstream | 188 Mbps  | ~460 Mbps |
| parallel40     | 878 Mbps  | —         |

`parallel40` showing only ~11% drop vs single-stream confirms the mutex itself is not
the bottleneck — many concurrent writers are fine. The problem is specifically the
deferred flush in the single-connection download path.

## Fix

In `crates/entry-stack/src/async_stack/tcp_stream.rs`, `poll_write` success arm:

```rust
Ok(n) => {
    tracing::trace!(bytes = n, "TcpStream send");
    let now = inner.now();
    inner.poll(now);   // emit immediately while lock is held
    drop(inner);
    self.shared.notify.notify_one();  // still needed for timers/ACKs/retransmits
    Poll::Ready(Ok(n))
}
```

`inner.now()` is a cheap `smoltcp::time::Instant::now()` call. `inner.poll(now)` runs
the smoltcp state machine and emits pending packets to the TUN device. This eliminates
the Tokio scheduler round-trip for the TX fast path.

The background poll loop still runs for:
- Retransmit timers
- ACK handling
- TUN ingress (RX path is unchanged)

## Steps

1. Apply the fix to `poll_write` in `tcp_stream.rs`
2. Run `cargo test -p wallhack-entry-stack` — existing tests should pass unchanged
3. Rebuild initrd (`just -f bench/bench.just build-initrd`) — also picks up the
   pending `tcp_upstream`/`tcp_downstream` rename in `init.sh`
4. Run benchmarks: `just bench` — expect `tcp_downstream` to improve significantly,
   `tcp_upstream` to be unchanged, parallel scenarios unchanged
5. Update TODO.md performance note (currently wrong — says "Likely in SyscallExitAdapter")

## TODO.md fix

Change the performance item from:
```
Likely in `SyscallExitAdapter` — investigate how TCP connections are managed in the exit
adapter, poll loop wakeup latency, and mutex contention between smoltcp writes and the
poll loop.
```
To reflect the actual root cause and fix location.

## What NOT to change

- `poll_read` — RX path already works correctly (epoll-driven, immediate)
- `poll_flush` — already calls `notify_one`, can optionally add `inner.poll(now)` too
  but it is not the bottleneck
- The poll loop itself — leave as-is, it still serves its purpose

## Commit plan

Two commits:
1. `chore: rename SessionInit→TcpStreamHeader, benchmark scenarios tcp_fwd→tcp_upstream`
   (all the naming work done this session — already compile-clean)
2. `fix(entry-stack): eager smoltcp flush in poll_write to reduce tx_downstream latency`
   (the actual perf fix)
