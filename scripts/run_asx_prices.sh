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

export STOCK_SYMBOLS="${STOCK_SYMBOLS:-BHP}"
export DATABASE_PATH="${DATABASE_PATH:-$PROJECT_ROOT/stocks.db}"
export RUN_ONCE=1

exec "$PROJECT_ROOT/target/release/stocks"
