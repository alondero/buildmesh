#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

BINARY="src-tauri/target/release/buildmesh"

if [[ "$(uname)" == "Darwin" ]]; then
  LOG_PATH="$HOME/Library/Application Support/com.alond.buildmesh/logs/buildmesh.log"
else
  LOG_PATH="${APPDATA:-$HOME}/com.alond.buildmesh/logs/buildmesh.log"
fi

# 1. Kill existing instances
if pgrep -f "$BINARY" > /dev/null 2>&1; then
  echo "Stopping existing buildmesh..."
  pkill -f "$BINARY" || true
  sleep 1
fi

# 2. Build (frontend + Rust)
echo "Building..."
npm run tauri build 2>&1 | tail -5

# 3. Verify binary exists
if [ ! -f "$BINARY" ]; then
  echo "ERROR: Build failed — $BINARY not found"
  exit 1
fi

# 4. Record log position
BEFORE_LINES=0
if [ -f "$LOG_PATH" ]; then
  BEFORE_LINES=$(wc -l < "$LOG_PATH")
fi

# 5. Launch raw binary (never the .app bundle — it can be stale)
"$BINARY" &
PID=$!
echo "Launched PID: $PID"

# 6. Verify via log
sleep 3
if [ -f "$LOG_PATH" ]; then
  NEW_LINES=$(tail -n +$((BEFORE_LINES + 1)) "$LOG_PATH")
  if echo "$NEW_LINES" | grep -qi "started\|ready"; then
    echo "OK — Buildmesh running"
    exit 0
  fi
fi

# Fallback: check process is alive
if kill -0 "$PID" 2>/dev/null; then
  echo "OK — Process alive (no log confirmation)"
  exit 0
fi

echo "ERROR: Buildmesh failed to start"
exit 1
