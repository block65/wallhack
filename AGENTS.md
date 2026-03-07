# Agent Guidelines for wallhack

Before starting any work, read the following standards from the `standards/` submodule:

- **Git & Workflow:** `./standards/workflow/git.md`
- **Rust Standards:** `./standards/lang/rust.md`
- **PR Workflow (TRIPLE):** `./standards/workflow/triple.md`

## Crate structure

- `crates/daemon` — daemon library (`wallhackd`): headless tunnel engine, accepts structured `DaemonConfig`
- `crates/cli` — unified CLI (`wallhack`/`wallhackd`): multi-call binary with daemon launcher, IPC control client, interactive REPL
- `crates/api` — REST API for entry node management (axum, behind `http-api` feature)
- `crates/core` — core logic
- `crates/exit-adapter` — exit node adapter trait + sessions
- `crates/entry-stack` — entry-side userspace TCP/IP stack (smoltcp-based, parses TUN packets into structured flows)
- `crates/transport`, `crates/wire` — supporting crates
- Slim build: `--no-default-features --features slim` (quic + websocket, no repl, no http-api)
- Default build: all features including `http-api` (axum REST API) and `repl` (interactive REPL)
- `wallhack-core` dep in `crates/daemon` must have `default-features = false` for feature isolation to work
- CLI crate forwards features (`quic`, `websocket`, `http-api`) to daemon; daemon has no CLI or tracing dependencies
- ICMP is `#[cfg(unix)]` only

## Toolchain

Rust nightly (`rust-toolchain.toml`). Edition 2024.

## Workspace

Cargo workspace with members in `crates/*`. The main crate is `wallhack` with
feature flags: `quic`, `websocket`, `http-api`, `full` (all three).

## Debugging

Use the `tracing` crate. Log levels are controlled via CLI flags — not `RUST_LOG`:

- `--debug [--debug-filter <substr>]` — DEBUG level, optional module filter
- `--trace [--trace-filter <substr>]` — TRACE level, optional module filter

Make decisions based on proof, not theory.

## Quality checks

Run `just check` from the repo root after finishing a task.

## OpenAPI spec

The spec is manually maintained at `website/src/data/openapi.json`. If you
add, remove, or change any route, request body, or response shape in
`crates/wallhack/src/api/`, update that file to match.

## Website

Follow the rules in `./website/WRITING.md`

## Naming Conventions: Topology and Peers
- The Protocol Exception: Standard terminology (client, server, send, receive)
  is allowed and expected when strictly interacting with underlying transport
  layers or standard APIs (e.g., initializing a QUIC connection, WebSocket
  servers, HTTP APIs).
- Prohibited terms (Domain Logic): When writing topology, routing, or
  peer-to-peer domain logic, do not use host, client, server, upstream,
  downstream, in, out, up, down, send, receive, reverse, or forward to describe
  data flows.
- Required terminology (Vectors): Describe data flows using absolute paths
  (source, destination, target) and concrete entities (peer, tun, device).
- Explicit identifiers: Code and logs must use explicit, fixed IDs (e.g., peer1,
  dmz1, nodeA). Do not use network roles as variable names.
- CLI consistency: eg REPL route add examples must explicitly include the --name
  <peer> flag on exit/relay commands to ensure routing examples remain
  self-documenting.

## TRIPLE PR Process for lead-agent only

When ready, lead agent will do this. Use `just --show <recipe>` to understand
first

PR: `just open-pr`
Merge: `just do-merge`

# Thou shalt not `git reset --hard` on a dirty tree.

If you know you need to `git stash` and the response is "nothing stashed" then
STOP. LOOK. LISTEN. something is wrong.