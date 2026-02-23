# Wallhack: Thin Client / Daemon Architecture Refactoring

## Branch
`refactor/rename-crates`

> NOTE: you can ignore existing clippy problems that will benefit from the
> refactor like "too many lines" or "too many arguments" just dont suppress
> them, ignore them                   
>
> You may also omit any `just check` to avoid benchmarks

## Source plan

`/home/mholman/.claude/plans/adaptive-enchanting-sundae.md`

## Completed
- **Phase 0a**: Rename all crates to `wallhack-*` prefix + move directories ✅
- **Phase 0b**: Extract `wallhack_core::api` → `wallhack-api` crate ✅
- **Phase 1a**: Define management protocol (`management.proto`) ✅
- **Phase 1b+1c**: `DaemonHandle` + refactor node startup ✅

### Crate layout (current)
```
crates/
├── core/              name = "wallhack-core"           use wallhack_core::
├── wire/              name = "wallhack-wire"            use wallhack_wire::
├── transport/         name = "wallhack-transport"       use wallhack_transport::
├── exit-adapter/      name = "wallhack-exit-adapter"    use wallhack_exit_adapter::
├── netstack/          name = "wallhack-netstack"        use wallhack_netstack::
├── api/               name = "wallhack-api"             ← Phase 0b (new crate)
└── cli/               name = "wallhack-cli"             use wallhack_cli::
```

---

## Phase 0b: Extract `wallhack_core::api` → `wallhack-api` crate

Move `crates/core/src/api/` → `crates/api/src/` as a new crate.

**What moves to `wallhack-api`:**
- `mod.rs` → `lib.rs` (router, `serve()`, security middleware)
- `handlers.rs` (axum route handlers)
- `auth.rs` (auth middleware)
- `state.rs` (`State`, `Event` types)
- `validation.rs` (host header validation)

**What stays in `wallhack-core`:**
- `node_api.rs` → moves to `crates/core/src/node_api.rs` (top-level module). This is the trait — it belongs in the core crate so both `wallhack-api` and direct library consumers can use it.

**New crate dependencies:**
- `wallhack-api` depends on: `wallhack-core` (for `NodeApi` trait + types), `axum`, `axum-server`, `tokio`, `tracing`
- `wallhack-core` drops: `axum`, `axum-server` deps and the `http-api` feature flag
- `wallhack-cli` depends on: `wallhack-api` (for REST, optional) and `wallhack-core` (for core types)

**Verification:** `cargo fmt --all && cargo check --workspace`

---

## Phase 1a: Define management protocol

New file: `crates/wire/proto/management.proto`

Length-delimited protobuf over Unix socket / named pipe. Bidirectional:
- Consumers send `ManagementRequest` (with `request_id`)
- Daemon sends `DaemonMessage` containing either a correlated `ManagementResponse` or an unsolicited `DaemonNotification`

Key messages: Ping, Status, Stats, Peers, Routes, AddRoute, RemoveRoute, Connect, Listen, Disconnect, Shutdown.

---

## Phase 1b+1c: `DaemonHandle` + refactor node startup

Create `crates/core/src/daemon.rs` with `DaemonHandle` struct.
Refactor entry/exit `run()` to spawn into a task and return the handle instead of blocking.

---

## Phase 1d: IPC listener

`crates/core/src/ipc.rs` — accepts connections on Unix domain socket, reads `ManagementRequest`, dispatches to `NodeApi`, writes `ManagementResponse`.

Socket path: `$XDG_RUNTIME_DIR/wallhack/wallhackd.sock` (fallback `/tmp/wallhack-$UID/wallhackd.sock`)

---

## Phase 2: Daemon binary + thin CLI

- `wallhackd` daemon: parses args → calls library → gets `DaemonHandle` → starts IPC listener → optionally starts REST API → runs until signal
- `wallhack` thin CLI: parse command → connect to Unix socket → send `ManagementRequest` → read `ManagementResponse` → display. Auto-start `wallhackd` if socket not found.

CLI depends only on `wallhack-wire` + `tokio`. No axum, no HTTP deps.

---

## Naming convention
- "client/server" only for things that are objectively servers (QUIC, WebSocket)
- Daemon/CLI relationship uses "daemon" and "CLI" or "REST API" terminology - avoid "client" when possible. Stop and ask if unsure.
- No Cargo aliases — `use wallhack_wire::` everywhere
