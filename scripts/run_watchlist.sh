#!/bin/bash
set -euo pipefail

PROJECT_ROOT="/Users/robmcnamara/Source/Stocks"
PATH="/Users/robmcnamara/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin"

cd "$PROJECT_ROOT"
mkdir -p "$PROJECT_ROOT/logs"

if [[ ! -x "$PROJECT_ROOT/target/release/stocks" ]]; then
  echo "Building release binary..."
  cargo build --release
fi

export WATCHLIST_ONLY=1
export WATCHLIST_SYMBOLS="${WATCHLIST_SYMBOLS:-BHP}"
export WATCHLIST_INTERVAL_SECS="${WATCHLIST_INTERVAL_SECS:-900}"

exec "$PROJECT_ROOT/target/release/stocks"
