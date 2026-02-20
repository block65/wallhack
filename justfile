set shell := ["bash", "-euo", "pipefail", "-c"]

website := justfile_directory() / "website"

# Pre-PR gate: fmt, lint, cargo build (slim + default), unit tests, website lint + build
check: fmt-check lint cargo-build test website-lint website-build

# cargo fmt --check
fmt-check:
    cargo fmt --all -- --check

# cargo clippy (all features)
lint:
    cargo clippy --all-features -- -D warnings

# Build slim and default profiles
cargo-build:
    cargo build --no-default-features --features slim
    cargo build

# Cargo unit tests
test:
    cargo test --all

# Install website dependencies (pnpm, frozen lockfile)
website-deps:
    cd {{website}} && pnpm install --frozen-lockfile --silent

# Website lint (biome)
website-lint: website-deps
    cd {{website}} && pnpm check

# Website build (astro)
website-build: website-deps
    cd {{website}} && pnpm build

# TRIPLE: Open a PR using TASK.md for title and body
open-pr:
    #!/usr/bin/env bash
    test -f TASK.md || { echo "TASK.md not found. Create it first (see TRIPLE.md)."; exit 1; }
    title=$(awk '/^# /{sub(/^# /, ""); print; exit}' TASK.md)
    gh pr create --title "$title" --body-file TASK.md

# TRIPLE: Merge the PR for the current branch (rebase merge)
do-merge:
    #!/usr/bin/env bash
    pr=$(gh pr view --json number --jq '.number')
    gh pr merge "$pr" --rebase --delete-branch
