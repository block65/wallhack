set shell := ["bash", "-euo", "pipefail", "-c"]

website := justfile_directory() / "website"

# Pre-PR gate: fmt, lint, cargo build (slim + default), unit tests, website lint + build
check: fmt-check lint cargo-build test website-lint website-build

# cargo fmt --check
fmt-check:
    cargo fmt --all -- --check

# cargo clippy matching CI: slim and default, all targets
lint:
    cargo clippy --all-targets --no-default-features --features slim -- -D warnings
    cargo clippy --all-targets -- -D warnings

# Build slim and default profiles
cargo-build:
    cargo build --no-default-features --features slim
    cargo build

# Cargo unit tests
test:
    cargo test --all

# Install website dependencies (pnpm, frozen lockfile)
website-deps:
    cd "{{website}}" && pnpm install --frozen-lockfile --silent

# Website lint (biome)
website-lint: website-deps
    cd "{{website}}" && pnpm check

# Website build (astro)
website-build: website-deps
    cd "{{website}}" && pnpm build

# Delete local branches that have been merged and deleted on origin
clean-branches:
    git fetch -p
    git branch -vv | awk '/: gone]/{print $1}' | xargs -r git branch -d

# TRIPLE: Open a PR using TASK.md for title and body
open-pr:
    #!/usr/bin/env bash
    set -euo pipefail
    test -f TASK.md || { echo "TASK.md not found. Create it first (see TRIPLE.md)."; exit 1; }
    title=$(awk '/^# /{sub(/^# /, ""); print; exit}' TASK.md)
    gh pr create --title "$title" --body-file TASK.md --

# TRIPLE: Merge the PR for the current branch (rebase merge)
do-merge:
    #!/usr/bin/env bash
    set -euo pipefail
    branch=$(git rev-parse --abbrev-ref HEAD)
    local=$(git rev-parse HEAD)
    remote=$(git rev-parse "origin/$branch" 2>/dev/null) || { echo "No remote tracking branch found. Push first."; exit 1; }
    [ "$local" = "$remote" ] || { echo "Local and remote are out of sync. Push first."; exit 1; }
    pr=$(gh pr view --json number --jq '.number') || { echo "No open PR found for this branch."; exit 1; }
    [ -n "$pr" ] || { echo "Could not determine PR number."; exit 1; }
    gh pr merge "$pr" --auto --rebase --delete-branch
    git checkout main
