# Agent Guidelines for wallhack

Before starting any work, read the following standards from the `standards/` submodule:

- **Git & Workflow:** `./standards/workflow/git.md`
- **Rust Standards:** `./standards/lang/rust.md`
- **PR Workflow (TRIPLE):** `./standards/workflow/triple.md`

## Crate structure

- `crates/cli` — binary entrypoint
- `crates/wallhack` — core logic
- `crates/exit-adapter` — exit node adapter trait + sessions
- `crates/transport`, `crates/netstack`, `crates/protobuf` — supporting crates
- Slim build: `--no-default-features --features slim` (quic + websocket, no readline, no http-api)
- Default build: all features including `http-api` (axum REST API)
- `wallhack` dep in `crates/cli` must have `default-features = false` for feature isolation to work
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

## Multi-agent safety

Multiple agents may be running concurrently. Before doing any git operation that
touches the working tree (checkout, merge, restore, stash pop), check for
uncommitted changes with `git status` first.

If another agent has uncommitted changes:
- **Do not** `git restore`, `git checkout -f`, or `git stash drop` those files
- To unblock a merge: use `gh pr merge` directly, then `git fetch upstream &&
  git merge --ff-only upstream/main` — do not rely on `just do-merge` if the
  working tree is dirty
- If a conflict is unavoidable, take the incoming (upstream) version for committed
  files and manually reapply the other agent's working-tree changes afterward
- `docs/tasks/` is managed by a dedicated tasks agent — never modify or delete
  files there

## Quality checks

Run `just check` from the repo root before opening a PR. It covers:
- `cargo fmt --check`
- `cargo clippy --all-features`
- cargo build (slim + default profiles)
- `cargo test --all`
- website lint (`biome check`) and build (`astro build`)

## OpenAPI spec

The spec is manually maintained at `website/src/data/openapi.json`. If you
add, remove, or change any route, request body, or response shape in
`crates/wallhack/src/api/`, update that file to match.

## Website

Follow the rules in `./website/WRITING.md`

### Peer naming in docs and examples

- Never use "host", "client", "upstream", or "downstream" to refer to a peer identifier.
- Never use directional or role-based language for peer IDs unless the role is absolute and unambiguous (e.g. `entry` or `exit` as node types, not as peer names).
- In code examples, use explicit peer IDs like `peer1`, `peer2`, `dmz1`, `node1` — names that are clearly wallhack peer identifiers, not network roles.
- Always show the `-i <peer_id>` flag on exit/relay commands when the REPL `route add` is demonstrated so examples are self-consistent.
