# Agent Guidelines for wallhack

Before starting any work, read the following standards from the `standards/` submodule:

- **Git & Workflow:** `./standards/workflow/git.md`
- **Rust Standards:** `./standards/lang/rust.md`

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

## Website

Follow the rules in `./website/WRITING.md`
