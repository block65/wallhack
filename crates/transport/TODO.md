# Transport Crate TODOs

This list tracks improvements and standards compliance tasks for the `wallhack-transport` crate.

## Current Structure (post-refactor)

```
src/
  lib.rs            — barrel only (mod + pub use)
  traits.rs         — Transport + BiStream trait definitions
  error.rs          — TransportError
  quic.rs           — QuicTransport, QuicBiStream
  websocket.rs      — websocket barrel (pub use from submodules)
  websocket/
    transport.rs    — WebSocketTransport, WebSocketTransportConfig
    driver.rs       — Driver, Command, TokioAsyncReadWrite
    streams.rs      — WebSocketBiStream, WebSocketSendStream, WebSocketRecvStream
    adapter.rs      — WebSocketByteStream
    upgrade.rs      — upgrade(), UpgradeError, UpgradeResult
```

## Completed

- [x] **Restructure crate root**: `traits.rs` for trait definitions; `lib.rs` as strict barrel
- [x] **Rename WS → WebSocket**: All `Ws*` types renamed to `WebSocket*`; callers in `wallhack-core` updated
- [x] **Reorganize into `websocket/` subdir**: Split into `transport.rs`, `driver.rs`, `streams.rs`, `adapter.rs`, `upgrade.rs`
- [x] **Timeout Configuration**: `PREFIX_READ_TIMEOUT` moved into `WebSocketTransportConfig.prefix_read_timeout`
- [x] **Tracing instrumentation**: `debug!` on connection/stream lifecycle, `error!` on yamux errors, `warn!` on prefix read timeouts/unknown prefixes, `debug!` on QUIC stream open/close
- [x] **QuicTransport error mapping**: `TimedOut` → `TransportError::Timeout`; `ApplicationClosed`/`LocallyClosed` → graceful `None`; others → `ConnectionClosed`

## Error Handling

- [x] **Standardize `Io` Errors**: `io::Error::other(e.to_string())` calls in `websocket/adapter.rs` now use `io::Error::other(e)` to preserve the source error chain via boxing.

## Refactoring & Hygiene

- [x] **Structured Concurrency in WS**: The `tokio::spawn` fire-and-forget in `driver.rs` `poll_inbound()` replaced with `FuturesUnordered<PrefixReadFut>` stored in `Driver`. Futures are now driven by `Driver::poll` and cancelled on `Driver` drop.

## Testing & Coverage Gaps

- [ ] **Test `QuicTransport`**: Add integration tests using `rcgen` for self-signed certs to verify connection and stream lifecycle.
- [x] **Test `WebSocketByteStream`**: partial reads, empty message skipping, non-binary frame skipping, large writes.
- [x] **Test Handshake Failures**: Added to `websocket/upgrade.rs`:
    - [x] Requests exceeding `MAX_REQUEST_SIZE`
    - [x] Missing `Sec-WebSocket-Key`
    - [x] Non-GET method
    - [x] Wrong `Sec-WebSocket-Version`
    - [x] Missing `Upgrade`/`Connection` headers (`NotWebSocket`)

## Performance & Benchmarking (Hot Path)

- [ ] **Add Criterion Benchmarks**:
    - [ ] Benchmark `WebSocketByteStream` read/write overhead
    - [ ] Benchmark stream multiplexing latency (QUIC vs yamux)
    - [ ] Benchmark memory allocation frequency during high-throughput transfers
- [ ] **Optimize `WebSocketByteStream`**: Investigate using `Bytes` more effectively to reduce copies in the read/write paths.

## Pre-existing issues (not introduced by this refactor)

- `wallhack-netstack` clippy errors in `tests.rs` (similar_names, items_after_statements) and doc_markdown warnings in `mod.rs` / `tcp_listener_any.rs` — these were present before the transport refactor.


## Other

- [ ] Refactor map_quic_connection_error: replace Option<()> sentinel with a status enum and use From impl for error conversion.