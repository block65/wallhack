# Migrate readline from rustyline to reedline

## Branch
`feat/reedline` — based on `feat/version-and-startup-ux`

## Scope
`crates/cli/` — `Cargo.toml`, `src/entry.rs`, `src/exit.rs`, `src/repl_common.rs`

The `#[cfg(not(feature = "repl"))]` non-REPL path must continue to work unchanged.
The feature flag is named `repl` (renamed from `readline` as part of this branch).

## Out of scope
Core wallhack logic, transports, netstack, protobuf, website, bench.
Do not change command parsing, REPL command set, or output formatting.

## Why

### Problems with the current rustyline implementation

The current readline implementation is broken. No REPL commands produce visible output.
The root cause is architectural: rustyline's `ExternalPrinter` only works correctly while
`rl.readline()` is actively blocking for input. Command responses are generated *after*
`readline()` returns (the command is sent to an async task, which processes it and sends
responses back through the print channel). By that point, `readline()` is no longer active,
so `ExternalPrinter` buffers the responses and only flushes them when `readline()` is called
again for the next prompt — after the prompt has already been drawn.

Concrete symptoms observed:
- Commands such as `peers`, `info`, `stats` produce no visible output
- Connection event messages (`[+] Peer connected: ...`) interleave with command responses
  in unpredictable order
- The prompt appears before command responses, making the terminal look broken

### The workaround that was attempted and failed

A `Done` sentinel (`PrintMsg::Done`) was introduced to signal command completion.
The readline thread waits with `done_rx.recv_timeout(500ms)` after sending a command,
hoping all response messages are queued in `ExternalPrinter` before the next
`readline()` call draws the prompt. This does not fix the problem because:

- `ExternalPrinter` still buffers when `readline()` is not active — messages arrive
  during the 500ms window but are not printed until the *next* `readline()` call
- The 500ms is a heuristic; fast commands signal `Done` before the user sees anything
- Background async events (peer connect/disconnect) arrive independently and race
  against the command response window
- The `DoneGuard` / `done_rx` machinery adds complexity and latency for zero benefit

### Why reedline fixes this

reedline (the Nushell readline library) uses a fundamentally different model:

- At the start of each `read_line()` call, reedline **flushes all pending
  `ExternalPrinter` messages before drawing the prompt**. This means command responses
  sent to the printer after `read_line()` returned will always appear correctly above the
  next prompt, regardless of async timing.
- The `ExternalPrinter` channel is decoupled from the event loop — messages sent at any
  time are buffered and printed at the correct moment.
- Designed explicitly for async-friendly REPLs (Nushell is async throughout).
- Signal handling (`Ctrl-C`, `Ctrl-D`) is first-class with a typed `Signal` return value.
- Active development; rustyline is comparatively stagnant.

## Goals

1. **Binary size check first — before any other work.** Measure the size delta of
   adding reedline vs rustyline. If the delta is unacceptable the migration may be
   abandoned or a lighter alternative chosen. Do not proceed to goal 2 until this
   is confirmed acceptable.
2. Replace rustyline with reedline in the `repl` feature path.
3. All existing REPL commands produce correct output in the correct order.
4. Background async events (peer connect/disconnect) print cleanly without corrupting
   the prompt line.
5. The non-REPL (`#[cfg(not(feature = "repl"))]`) path is unchanged.
6. Document the binary size delta in this file once measured.

## Known challenges

### Binary size
reedline pulls in more dependencies than rustyline. The slim build (`--features slim`,
no readline) must be unaffected. The full build will grow; the question is how much.
`cargo bloat --release --crates` can give per-crate size breakdown.

### PrintMsg / DoneGuard compatibility
The current `PrintMsg { Text(String), Done }` enum and `DoneGuard` RAII were designed
as a rustyline workaround. With reedline the `Done` sentinel may become unnecessary for
command responses. However, the non-readline path also uses `PrintMsg` and must continue
to work. Changing `PrintMsg` affects both paths.

### Printer channel type
`Printer` wraps `mpsc::UnboundedSender<PrintMsg>`. reedline's `ExternalPrinter` accepts
`String` not `PrintMsg`. The channel and `Printer` abstraction will need to bridge these.

### Single vs two printers
Currently one `Printer` is used for both REPL command responses and background async
events. With reedline's model, it may be necessary or desirable to separate these two
concerns so each can be routed differently.

### REPL feature gate
The `repl` feature (renamed from `readline` in this branch) is referenced in `Cargo.toml`
and guarded with `#[cfg(feature = "repl")]` in `entry.rs` and `exit.rs`. reedline must
slot into the same feature gate.

### Blocking thread model
The readline loop runs in a `spawn_blocking` thread to avoid blocking the async runtime.
reedline's `read_line()` is synchronous, so this model is preserved. Verify that
reedline does not spawn its own tokio runtime or conflict with the existing one.

### History, completions, hints
rustyline history (`add_history_entry`) is used today. reedline has its own history API.
Completions and hints are not implemented today — preserve the same capability gap; do
not add them as part of this migration.
