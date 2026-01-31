#!/usr/bin/env bash
set -euo pipefail

# Wallhack full test and benchmark suite
# Outputs results to bench/results/

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
RESULTS_DIR="$SCRIPT_DIR/results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULT_FILE="$RESULTS_DIR/benchmark_$TIMESTAMP.txt"

mkdir -p "$RESULTS_DIR"

log() {
    echo "[$(date +%H:%M:%S)] $*" | tee -a "$RESULT_FILE"
}

separator() {
    echo "============================================================" | tee -a "$RESULT_FILE"
}

cd "$ROOT_DIR"

separator
log "WALLHACK TEST & BENCHMARK SUITE"
log "Started: $(date)"
log "Git commit: $(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
separator

# Build
log ""
log "BUILDING..."
cargo build -q --release -p repl --features websocket 2>&1 | tee -a "$RESULT_FILE"
log "Build complete"

# Binary info
log ""
log "BINARY INFO:"
ls -lh target/release/wallhack | tee -a "$RESULT_FILE"
file target/release/wallhack | tee -a "$RESULT_FILE"

# Cargo tests
separator
log ""
log "CARGO TESTS..."
cargo test --release 2>&1 | tee -a "$RESULT_FILE"

# Clippy
separator
log ""
log "CLIPPY..."
cargo clippy --all-targets --all-features 2>&1 | tee -a "$RESULT_FILE" || true

# Benchmarks (requires sudo)
separator
log ""
log "BENCHMARKS (requires sudo)..."

if [ "$EUID" -ne 0 ]; then
    log "Running benchmarks with sudo..."
    SUDO="sudo -E"
else
    SUDO=""
fi

# Kill any stale wallhack processes
$SUDO pkill -9 -f 'wallhack.*(-l|-c)' 2>/dev/null || true
sleep 1

cd "$SCRIPT_DIR"

# QUIC benchmarks
log ""
log "QUIC TRANSPORT BENCHMARKS:"
$SUDO pytest tests/test_benchmark_parallel.py -v -s 2>&1 | \
    grep -E "(PASS|FAIL|Mbps|test_|ERROR)" | tee -a "$RESULT_FILE"

# WebSocket benchmarks  
log ""
log "WEBSOCKET TRANSPORT BENCHMARKS:"
$SUDO pytest tests/test_benchmark_websocket.py -v -s 2>&1 | \
    grep -E "(PASS|FAIL|Mbps|test_|ERROR)" | tee -a "$RESULT_FILE"

# Cleanup
$SUDO pkill -9 -f 'wallhack.*(-l|-c)' 2>/dev/null || true

separator
log ""
log "RESULTS SUMMARY"
separator
log "Completed: $(date)"
log "Results saved to: $RESULT_FILE"

# Extract key metrics
log ""
log "KEY METRICS:"
grep -E "Mbps" "$RESULT_FILE" | tail -20

echo ""
echo "Full results: $RESULT_FILE"
