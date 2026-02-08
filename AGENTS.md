# Agent Guidelines

## Build & Verification

Always use `-q` (quiet) with cargo commands to reduce noise:

```sh
cargo build -q --features full
cargo clippy -q --features full --fix --allow-dirty
cargo test -q
cargo fmt
```

Run all four after making changes. Fix any issues before committing.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/). Keep messages
short — explain **why**, not what.

Prefixes: `fix:`, `feat:`, `refactor:`, `chore:`, `build:`, `docs:`, `style:`,
`test:`

Commit messages explain **why**, not what — git already tracks what changed.
Bad: `feat(tls): add FingerprintVerifier struct`
Good: `feat(tls): support TOFU certificate pinning via SHA-256 fingerprint`

Each commit must be a single logical unit of related work. Split unrelated
changes into separate commits. Stage related hunks and files together — do not
`git add -A` everything into one commit. Format/lint fixes go in their own
`chore: lint` commit.

Optional scope in parentheses when it clarifies the area: `feat(tls):`,
`fix(exit):`, `refactor(proto):`, etc. Use the crate or module name as scope.

Examples from this repo:

```
fix: avoid panicking on unimplemented code paths
fix: prevent timing side-channel in credential validation
chore: remove dead code and commented-out blocks
build: optimize release profile for size and determinism
refactor: use default port in examples and remove directional terminology
chore: suppress clippy warnings in appropriate places
```

## Toolchain

Rust nightly (`rust-toolchain.toml`). Edition 2024.

## Workspace

Cargo workspace with members in `crates/*`. The main crate is `wallhack` with
feature flags: `quic`, `websocket`, `api`, `full` (all three).

## Debugging

Use the `tracing` crate and `RUST_LOG` environment to help you see whats going
on during tests and benchmarks, make decisions based on proof, not theory.
