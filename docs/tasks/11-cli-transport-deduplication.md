# CLI Transport Deduplication

`cli/src/entry.rs` (1258 lines) and `cli/src/exit.rs` (1602 lines) are the
two largest files in the codebase and are structurally broken: QUIC and
WebSocket paths are copy-pasted rather than parameterized, REPL input is
duplicated across feature flags, and REPL dispatch logic is scattered.

This task is a clean-sheet internal rewrite of the CLI modules. The public
user-facing CLI surface (subcommands, flags, help text) stays the same.

## Scope

`crates/cli/src/entry.rs`, `crates/cli/src/exit.rs`, `crates/cli/src/relay.rs`,
new `crates/cli/src/repl.rs`

---

## The Problem

### QUIC/WS duplication (~500 lines)

- `run_quic_exit` / `run_ws_exit` (exit.rs) — ~90 lines each, identical except
  for client constructor and config type
- `run_quic_relay_capability` / `run_ws_relay_capability` — ~130 lines each
- Same pattern in entry.rs for `run_entry_listen` / `run_entry_connect`
- In relay.rs for relay capability functions

Every new protocol feature must be implemented N times.

### REPL duplication (~400 lines)

- `run_exit_repl_input` and `run_entry_repl_input` each have two copies: one
  with `#[cfg(feature = "readline")]` and one without
- REPL dispatch (`match cmd { ... }`) is copy-pasted across 5 locations in
  exit.rs with slight variations

### 8-parameter `handle_connection`

`entry.rs:970` even has a `// TODO` admitting it needs a `ConnectionContext`
struct.

---

## Plan

### 1. Extract a shared REPL module (`cli/src/repl.rs`)

Define a `ReplInput` trait:
```rust
pub trait ReplInput: Send {
    async fn next_line(&mut self, prompt: &str) -> Option<String>;
}
```

Implement it for:
- `ReadlineReplInput` (wraps `rustyline`) — `#[cfg(feature = "readline")]`
- `PlainReplInput` (wraps stdin lines) — fallback

The REPL dispatch loop becomes a single `async fn run_repl<R: ReplInput>(...)`.
Both entry and exit import it.

### 2. Parameterize run_*_exit and run_*_entry over transport

Define a `ClientFactory` trait (or use a closure/enum):
```rust
trait ClientFactory: Send + 'static {
    type Client: Transport;
    async fn connect(&self) -> Result<Self::Client, ...>;
}
```

`run_exit` becomes:
```rust
async fn run_exit<F: ClientFactory>(factory: F, config: ExitConfig) -> Result<(), Error>
```

The two `run_quic_exit` / `run_ws_exit` functions collapse into one generic
call each. Callers select the factory at the `#[cfg(feature)]` boundary, not
inside the function body.

### 3. `ConnectionContext` struct

Replace the 8-parameter `handle_connection` with:
```rust
struct ConnectionContext {
    transport: Arc<dyn Transport>,
    metrics: SharedMetrics,
    instructions_tx: mpsc::Sender<EntryNodeInstruction>,
    responses_rx: mpsc::Receiver<ExitNodeResponse>,
    // ... etc
}
```

### 4. Feature-flag boundary consolidation

All `#[cfg(feature = "quic")]` and `#[cfg(feature = "websocket")]` blocks
should be confined to the *factory selection* site — a small match or if-else
at the top of `run_entry` / `run_exit` that picks the factory, then calls the
generic function. No cfg blocks inside the function bodies.

---

## Approach

This is a large refactor. Recommended PR sequence:

1. **Extract `repl.rs`** — purely additive, no deleted code yet
2. **Route entry REPL through `repl.rs`**, delete the two old copies
3. **Route exit REPL through `repl.rs`**, delete the two old copies
4. **Introduce `ConnectionContext`** and thread it through `handle_connection`
5. **Parameterize `run_exit`** over transport, delete the QUIC/WS copies
6. **Parameterize `run_entry`** the same way
7. **Clean up relay.rs**

## Acceptance criteria

- `cli/src/entry.rs` and `cli/src/exit.rs` each under 600 lines
- Only one `ReplInput` loop implementation exists (in `repl.rs`)
- `#[cfg(feature = "quic")]` / `#[cfg(feature = "websocket")]` blocks only
  at factory-selection sites, not inside business-logic functions
- `just check` passes on both slim and default builds
- `#[allow(clippy::too_many_arguments)]` and `#[allow(clippy::too_many_lines)]`
  suppresses are deleted
