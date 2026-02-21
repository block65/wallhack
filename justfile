set shell := ["bash", "-euo", "pipefail", "-c"]

website     := justfile_directory() / "website"
bench       := justfile_directory() / "bench"
iperf3_bin  := bench / "bin" / "iperf3"
iperf3_ver  := "3.20"

# Pre-PR gate: fmt, lint, build, unit tests, smoke, resilience, website lint + build
check: fmt-check lint cargo-build test smoke resilience website-lint website-build

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

# Build release binary with all transports (required by VM tests)
build-release:
    cargo build --release --features full

# Install website dependencies (pnpm, frozen lockfile)
website-deps:
    cd "{{website}}" && pnpm install --frozen-lockfile --silent

# Website lint (biome)
website-lint: website-deps
    cd "{{website}}" && pnpm check

# Website build (astro)
website-build: website-deps
    cd "{{website}}" && pnpm build

# VM integration tests: basic tunnel connectivity (QUIC + WebSocket)
smoke: build-release
    python3 "{{bench}}/run_tests.py" smoke

# VM integration tests: degraded network resilience (QUIC + WebSocket)
resilience: build-release
    python3 "{{bench}}/run_tests.py" resilience

# VM integration benchmarks: throughput + latency (not in `just check`)
benchmark: build-release fetch-iperf3
    python3 "{{bench}}/run_benchmarks.py"

# Boot both VMs interactively for manual topology inspection
debug-topology: build-release
    python3 "{{bench}}/run_tests.py" debug-topology

# Build base VM image (run once; requires qemu + cloud-image-utils)
setup-vm:
    bash "{{bench}}/setup-vm.sh"

# Download static iperf3 binary for benchmark VMs
fetch-iperf3:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -x "{{iperf3_bin}}" ]]; then
        echo "iperf3 already present"
        exit 0
    fi
    mkdir -p "$(dirname "{{iperf3_bin}}")"
    url="https://github.com/userdocs/iperf3-static/releases/download/{{iperf3_ver}}/iperf3-amd64"
    echo "Downloading iperf3 {{iperf3_ver}}..."
    curl -fsSL -o "{{iperf3_bin}}" "$url"
    chmod +x "{{iperf3_bin}}"
    echo "iperf3 downloaded to {{iperf3_bin}}"

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
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "Uncommitted changes present — commit or stash before merging."
        git status --short
        exit 1
    fi
    branch=$(git rev-parse --abbrev-ref HEAD)
    local=$(git rev-parse HEAD)
    remote=$(git rev-parse "origin/$branch" 2>/dev/null) || { echo "No remote tracking branch found. Push first."; exit 1; }
    [ "$local" = "$remote" ] || { echo "Local and remote are out of sync. Push first."; exit 1; }
    pr=$(gh pr view --json number --jq '.number') || { echo "No open PR found for this branch."; exit 1; }
    [ -n "$pr" ] || { echo "Could not determine PR number."; exit 1; }
    gh pr merge "$pr" --rebase --delete-branch
    git fetch upstream
    git checkout main
    git merge --ff-only upstream/main
    git push origin main
