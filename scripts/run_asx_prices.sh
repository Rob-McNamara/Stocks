#!/bin/bash
set -euo pipefail

PROJECT_ROOT="/Users/robmcnamara/Source/Stocks"
PATH="/Users/robmcnamara/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin"

cd "$PROJECT_ROOT"
mkdir -p "$PROJECT_ROOT/logs"

# Always build: cargo does nothing when the binary is current, and building
# only when it was missing meant code changes never reached the scheduled run.
cargo build --release --quiet --bin stocks

# ASX symbols need the .AX suffix; bare "BHP" is the US-listed ADR.
export STOCK_SYMBOLS="${STOCK_SYMBOLS:-BHP.AX}"
export DATABASE_PATH="${DATABASE_PATH:-$PROJECT_ROOT/stocks.db}"
export RUN_ONCE=1

exec "$PROJECT_ROOT/target/release/stocks"
