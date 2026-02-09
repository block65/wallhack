#!/usr/bin/env bash
set -euo pipefail

# Binary size bloat checker for wallhack
# Builds multiple feature/target combinations and checks against size thresholds.
# Use as a pre-release sanity check or in CI to catch unexpected size regressions.
#
# Usage:
#   ./bench/check_bloat.sh              # build all variants
#   ./bench/check_bloat.sh --quick      # native glibc only (faster)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
RESULTS_DIR="$SCRIPT_DIR/results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULT_FILE="$RESULTS_DIR/bloat_$TIMESTAMP.txt"

mkdir -p "$RESULTS_DIR"

cd "$ROOT_DIR"

# --- Size thresholds (bytes) ---
# Updated: 2026-02-09, baseline commit: $(git rev-parse --short HEAD 2>/dev/null)
# Set ~10% above current measured sizes. Adjust as features are added.
declare -A THRESHOLDS=(
    # glibc x86_64 (~10% headroom)
    ["glibc-slim"]=5200000         # current: ~4.7M
    ["glibc-default"]=5500000      # current: ~4.9M
    # musl x86_64 (~10% headroom)
    ["musl-slim"]=5200000          # current: ~4.6M
    ["musl-default"]=5500000       # current: ~4.9M
)

# --- Build definitions ---
# format: "label|target|cargo_features"
BUILDS_ALL=(
    "glibc-slim|x86_64-unknown-linux-gnu|--no-default-features --features full"
    "glibc-default|x86_64-unknown-linux-gnu|"
    "musl-slim|x86_64-unknown-linux-musl|--no-default-features --features full"
    "musl-default|x86_64-unknown-linux-musl|"
)

BUILDS_QUICK=(
    "glibc-slim|x86_64-unknown-linux-gnu|--no-default-features --features full"
    "glibc-default|x86_64-unknown-linux-gnu|"
)

# Parse args
MODE="all"
case "${1:-}" in
    --quick) MODE="quick" ;;
    --help|-h)
        echo "Usage: $0 [--quick]"
        echo "  (none)   Build all variants (glibc + musl, slim to full)"
        echo "  --quick  Native glibc only (2 builds)"
        exit 0
        ;;
esac

case "$MODE" in
    quick) BUILDS=("${BUILDS_QUICK[@]}") ;;
    *)     BUILDS=("${BUILDS_ALL[@]}") ;;
esac

# --- Run ---
FAILED=0
PASS_COUNT=0

log() {
    echo "$*" | tee -a "$RESULT_FILE"
}

log "WALLHACK BINARY SIZE CHECK"
log "Date: $(date)"
log "Commit: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
log "Mode: $MODE"
log ""
printf "%-16s %-30s %10s %10s %s\n" "VARIANT" "TARGET" "SIZE" "LIMIT" "STATUS" | tee -a "$RESULT_FILE"
printf "%-16s %-30s %10s %10s %s\n" "-------" "------" "----" "-----" "------" | tee -a "$RESULT_FILE"

for build in "${BUILDS[@]}"; do
    IFS='|' read -r label target features <<< "$build"

    # shellcheck disable=SC2086
    if ! cargo build -q --release -p cli --target "$target" $features 2>&1; then
        log "$(printf '%-16s %-30s %10s %10s %s' "$label" "$target" "BUILD FAIL" "-" "FAIL")"
        FAILED=$((FAILED + 1))
        continue
    fi

    binary="target/$target/release/wallhack"
    size=$(stat --format='%s' "$binary" 2>/dev/null || stat -f'%z' "$binary")
    threshold=${THRESHOLDS[$label]:-0}

    size_mb=$(awk "BEGIN {printf \"%.1fM\", $size/1048576}")
    limit_mb=$(awk "BEGIN {printf \"%.1fM\", $threshold/1048576}")

    if [ "$threshold" -gt 0 ] && [ "$size" -gt "$threshold" ]; then
        status="FAIL (+$(awk "BEGIN {printf \"%.0f\", ($size-$threshold)/$threshold*100}")%)"
        FAILED=$((FAILED + 1))
    else
        status="ok"
        PASS_COUNT=$((PASS_COUNT + 1))
    fi

    log "$(printf '%-16s %-30s %10s %10s %s' "$label" "$target" "$size_mb" "$limit_mb" "$status")"
done

log ""
log "Results: $PASS_COUNT passed, $FAILED failed"
log "Saved to: $RESULT_FILE"

if [ "$FAILED" -gt 0 ]; then
    log ""
    log "BLOAT CHECK FAILED - binary size exceeds threshold"
    exit 1
fi
