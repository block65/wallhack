#!/usr/bin/env bash
set -euo pipefail

# Binary size bloat checker for wallhack
# Builds multiple feature/target combinations and checks against size thresholds.
# Use as a pre-release sanity check or in CI to catch unexpected size regressions.
#
# Usage:
#   ./bench/check_bloat.sh              # build all variants
#   ./bench/check_bloat.sh --quick      # native glibc only (faster)
#   ./bench/check_bloat.sh --no-build   # check sizes of already-built artifacts, skip missing

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
RESULTS_DIR="$SCRIPT_DIR/results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULT_FILE="$RESULTS_DIR/bloat_$TIMESTAMP.txt"

mkdir -p "$RESULTS_DIR"

cd "$ROOT_DIR"

# --- Size thresholds (bytes) ---
# DO NOT BUMP WITHOUT PERMISSION.
# Updated: 2026-03-16, baseline: v0.8.1 + relay fix + vergen
# Slim grew from auto-negotiation + relay monomorphization (see bench/results/bloat_20260301_analysis.txt).
# Real fix requires core-level ErasedTransport refactor (tracked in TODO).
declare -A THRESHOLDS=(
    # musl x86_64 — PRIMARY build target
    ["slim-musl"]=5505024          # 5.25MiB; current: ~5.35M
    ["default-musl"]=7500000       # current: ~7.1M (~5% headroom)
    # glibc x86_64 — sanity check only
    ["slim-glibc"]=5505024         # 5.25MiB; current: ~5.35M
    ["default-glibc"]=7500000      # current: ~7.1M (~5% headroom)
)

# --- Build definitions ---
# format: "label|target|cargo_features"
BUILDS_ALL=(
    "default-glibc|x86_64-unknown-linux-gnu|"
    "slim-glibc|x86_64-unknown-linux-gnu|--no-default-features --features slim"
    "default-musl|x86_64-unknown-linux-musl|"
    "slim-musl|x86_64-unknown-linux-musl|--no-default-features --features slim"
)

BUILDS_QUICK=(
    "default-glibc|x86_64-unknown-linux-gnu|"
    "slim-glibc|x86_64-unknown-linux-gnu|--no-default-features --features slim"
)

# Parse args
MODE="all"
NO_BUILD=false
ONLY=""
for arg in "$@"; do
    case "$arg" in
        --quick)    MODE="quick" ;;
        --no-build) NO_BUILD=true ;;
        --only=*)   ONLY="${arg#--only=}" ;;
        --help|-h)
            echo "Usage: $0 [--quick] [--no-build] [--only=LABEL]"
            echo "  (none)        Build all variants (glibc + musl)"
            echo "  --quick       Native glibc only (2 builds)"
            echo "  --no-build    Skip builds; check sizes of existing artifacts only"
            echo "  --only=LABEL  Only check the named variant (e.g. --only=slim-glibc)"
            exit 0
            ;;
    esac
done

case "$MODE" in
    quick) BUILDS=("${BUILDS_QUICK[@]}") ;;
    *)     BUILDS=("${BUILDS_ALL[@]}") ;;
esac

# --- Run ---
FAILED=0
PASS_COUNT=0
SKIP_COUNT=0

log() {
    echo "$*" | tee -a "$RESULT_FILE"
}

log "WALLHACK BINARY SIZE CHECK"
log "Date: $(date)"
log "Commit: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
log "Mode: $MODE${NO_BUILD:+ (no-build)}"
log ""
printf "%-16s %-30s %10s %12s %10s %s\n" "VARIANT" "TARGET" "SIZE" "BYTES" "LIMIT" "STATUS" | tee -a "$RESULT_FILE"
printf "%-16s %-30s %10s %12s %10s %s\n" "-------" "------" "----" "-----" "-----" "------" | tee -a "$RESULT_FILE"

for build in "${BUILDS[@]}"; do
    IFS='|' read -r label target features <<< "$build"

    [ -z "$ONLY" ] || [ "$label" = "$ONLY" ] || continue

    if [[ "$target" == *-musl ]]; then
        build_cmd="cross"
    else
        build_cmd="cargo"
    fi
    binary="target/$target/release/wallhack"

    if [ "$NO_BUILD" = "true" ]; then
        if [ ! -f "$binary" ]; then
            log "$(printf '%-16s %-30s %10s %10s %s' "$label" "$target" "-" "-" "skip")"
            SKIP_COUNT=$((SKIP_COUNT + 1))
            continue
        fi
    else
        # shellcheck disable=SC2086
        if ! $build_cmd build -q --release -p wallhack-cli --target "$target" $features 2>&1; then
            log "$(printf '%-16s %-30s %10s %10s %s' "$label" "$target" "BUILD FAIL" "-" "FAIL")"
            FAILED=$((FAILED + 1))
            continue
        fi
    fi

    size=$(stat --format='%s' "$binary" 2>/dev/null || stat -f'%z' "$binary")
    threshold=${THRESHOLDS[$label]:-0}

    size_mb=$(awk "BEGIN {printf \"%.2fM\", $size/1048576}")
    limit_mb=$(awk "BEGIN {printf \"%.2fM\", $threshold/1048576}")

    if [ "$threshold" -gt 0 ] && [ "$size" -gt "$threshold" ]; then
        status="FAIL (+$(awk "BEGIN {printf \"%.0f\", ($size-$threshold)/$threshold*100}")%)"
        FAILED=$((FAILED + 1))
    else
        status="ok"
        PASS_COUNT=$((PASS_COUNT + 1))
    fi

    log "$(printf '%-16s %-30s %10s %12s %10s %s' "$label" "$target" "$size_mb" "$size" "$limit_mb" "$status")"
done

log ""
log "Results: $PASS_COUNT passed, $FAILED failed, $SKIP_COUNT skipped"

# --- Crate breakdown (glibc only, requires cargo-bloat) ---
if command -v cargo-bloat >/dev/null 2>&1 && [ "$NO_BUILD" = "false" ]; then
    for build in "${BUILDS[@]}"; do
        IFS='|' read -r label target features <<< "$build"
        [ -z "$ONLY" ] || [ "$label" = "$ONLY" ] || continue
        # cargo-bloat can't analyse cross-compiled binaries
        [[ "$target" != *-musl ]] || continue

        log ""
        log "=== Top 30 crates: $label ==="
        # cargo-bloat doesn't support --quiet, but respects CARGO_TERM_QUIET
        # shellcheck disable=SC2086
        CARGO_TERM_QUIET=true cargo bloat --release -p wallhack-cli --target "$target" $features --crates -n 30 2>&1 \
            | grep -E '^\s+\S|^ File' | tee -a "$RESULT_FILE"
    done
fi

log ""
log "Saved to: $RESULT_FILE"

if [ "$FAILED" -gt 0 ]; then
    log ""
    log "BLOAT CHECK FAILED - binary size exceeds threshold"
    exit 1
fi
