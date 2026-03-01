# Eliminate the Dual TCP Forwarding Path

There are currently two complete, incompatible TCP forwarding implementations.
Pick one, delete the other.

## Scope

`crates/wallhack/src/entry/session.rs`,
`crates/wallhack/src/exit/orchestrator.rs`,
`crates/exit-adapter/src/`,
`crates/cli/src/exit.rs`

---

## The Two Paths

### Path A — Stream-per-session (entry/session.rs + cli/exit.rs handle_stream)

1. Entry: `run_tcp_session` opens a QUIC bi-stream per TCP connection
2. Entry: writes `SessionInit` protobuf, reads `SessionStatus`
3. Entry: runs `copy_bidirectional` between the smoltcp stream and the QUIC stream
4. Exit: `handle_stream` (cli/src/exit.rs:651) reads `SessionInit`, calls
   `TcpStream::connect` directly, runs `copy_bidirectional`

This path bypasses the `ExitAdapter` trait and the `Orchestrator` entirely.

### Path B — Orchestrator/adapter path

1. Entry: sends `EntryNodeInstruction::TcpConnect` through the data channel
2. Orchestrator on exit: calls `adapter.tcp_connect()`, `adapter.tcp_send()`,
   `adapter.tcp_close()` for every operation
3. Every byte of data is serialised into a protobuf, sent over the channel,
   deserialised, written to a socket

This path uses the `ExitAdapter` trait, adds protobuf overhead for every TCP
byte, and has `tcp_listen` returning `Err(Unsupported)` (unimplemented).

---

## Recommendation: Keep Path A, Delete Path B for TCP

Path A is correct for TCP:
- Stream-per-connection is idiomatic QUIC usage (QUIC streams are cheap)
- `copy_bidirectional` is efficient — no serialisation overhead per byte
- It is already working and tested

Path B makes sense for **UDP** (where multiplexing many flows over a shared
channel is genuinely useful — hence the udp-unified-orchestrator refactor
currently underway). TCP doesn't benefit from the same model.

---

## Plan

### 1. Confirm Path A handles all TCP cases

Audit `handle_stream` in cli/src/exit.rs to ensure it handles:
- Connection refused → `SessionStatus::ConnectionRefused`
- Host unreachable → `SessionStatus::ConnectionRefused`
- Successful connect → `SessionStatus::Success` + `copy_bidirectional`
- Half-close (FIN in one direction) → propagated via `copy_bidirectional`

Fix any gaps before removing Path B.

### 2. Remove TCP from the Orchestrator

In `exit/orchestrator.rs`, remove:
- `TcpConnect`, `TcpSend`, `TcpClose`, `TcpListen`, `TcpListenClose` dispatch arms
- The `run_tcp_recv` helper and related session management
- Any TCP-related fields on the Orchestrator

The Orchestrator should only handle UDP (and ICMP if/when implemented).

### 3. Remove TCP methods from ExitAdapter

In `exit-adapter/src/adapter.rs`, remove from the `ExitAdapter` trait:
- `tcp_connect()`
- `tcp_send()`
- `tcp_recv_session()`
- `tcp_close()`
- `tcp_listen()` (already returns `Err(Unsupported)`)
- `tcp_listen_close()` (already returns `Err(Unsupported)`)

Remove from `SyscallExitAdapter`:
- `crates/wallhack/src/exit/net/tcp.rs` — the entire file

### 4. Remove TCP response types from exit-adapter

In `exit-adapter/src/adapter.rs`, delete:
- `TcpStreamResponse` enum
- `TcpCloseResponse` enum
- `TcpListenResponse` enum
- `TcpListenCloseResponse` enum
- Their `From` impl blocks into protobuf types

### 5. Remove TCP variants from EntryNodeInstruction / ExitNodeResponse protobufs

In `crates/protobuf/`, remove:
- `EntryNodeInstruction::TcpConnect`, `TcpSend`, `TcpClose` if present
- `ExitNodeResponse::TcpStream`, `TcpClose` etc.

The protobuf schema is the source of truth — change `.proto` files and
regenerate, or edit the `prost`-generated types if `.proto` files are managed
externally.

---

## Notes

- This is a **follow-on to the udp-unified-orchestrator branch** — ensure that
  branch is merged first so UDP is stable on the orchestrator path before
  removing TCP from it.
- The `exit-adapter` crate may become significantly smaller after this —
  potentially only UDP and ICMP session management remain. Evaluate whether
  it still warrants a separate crate or can be inlined into `wallhack`.
- `NullExitAdapter` in test helpers can be simplified after TCP removal.

## Acceptance criteria

- `grep -r "TcpConnect\|TcpSend\|TcpClose\|TcpListen" crates/` returns no hits
  outside of `cli/src/exit.rs` (the `handle_stream` path)
- TCP tunnelling still works end-to-end (integration test)
- `just check` passes
- The `exit-adapter` crate compiles with only UDP and ICMP session types

## Implementation Tips

- **Locating Path A**: The current Path A implementation is in `crates/daemon/src/mode/exit.rs` (look for the `handle_stream` function) and `crates/core/src/entry/session.rs`.
- **Locating Path B**: Path B's logic is primarily in `crates/core/src/exit/orchestrator.rs` (dispatching logic) and `crates/core/src/exit/net/tcp.rs` (the `SyscallExitAdapter` implementation).
- **Protobuf Strategy**: When removing fields from `data.proto`, do not renumber existing fields. Simply comment out or delete the TCP-related fields in `EntryNodeInstruction` and `ExitNodeResponse`.
- **Mock/Null Adapters**: Remember to update `crates/exit-adapter/src/tests_helpers/mock_exit.rs`. It contains a `NullExitAdapter` that currently implements the dead TCP methods.
- **Verification**: Run `cargo test --package wallhack-core` and `cargo test --package wallhack-daemon` after changes. The integration tests in `range/` are the ultimate source of truth for TCP forwarding correctness.
- **Recommended Order**:
    1.  Remove TCP dispatch arms from `orchestrator.rs`.
    2.  Remove TCP methods from the `ExitAdapter` trait in `crates/exit-adapter/src/adapter.rs`.
    3.  Delete `crates/core/src/exit/net/tcp.rs` and references in `crates/core/src/exit/net/mod.rs`.
    4.  Update the Protobuf definitions and regenerate code.
