#!/usr/bin/env bash
# Builds and launches the DEV profile (buildmesh-dev) so it runs side-by-side
# with the stable hub (buildmesh) without interrupting its agents. This is what
# /use and /verify call. It only ever touches buildmesh-dev — never the hub.
set -euo pipefail

cd "$(dirname "$0")/.."

# The dev profile must NOT share src-tauri/target/release/ with the stable
# hub: cargo's binary output filename is fixed by the crate's [[bin]] name
# ("buildmesh"), so both profiles would write to buildmesh.exe. On Windows
# the stable hub holds that file open and the dev build fails with
# "Access is denied"; on macOS/Linux a parallel collision is still
# possible. Pointing CARGO_TARGET_DIR at a separate release-dev/ subdir
# gives the dev build its own buildmesh-dev binary (via Tauri's
# mainBinaryName overlay) and keeps the lock contention away from the hub.
# When CARGO_TARGET_DIR is set, cargo nests the profile subdir
# (`<target>/<profile>/<binary>`), so the release build drops the exe at
# release-dev/release/buildmesh-dev — not directly under release-dev/.
export CARGO_TARGET_DIR="src-tauri/target/release-dev"
BINARY="$CARGO_TARGET_DIR/release/buildmesh-dev"

if [[ "$(uname)" == "Darwin" ]]; then
  LOG_PATH="$HOME/Library/Application Support/com.alond.buildmesh.dev/logs/buildmesh.log"
else
  LOG_PATH="${APPDATA:-$HOME}/com.alond.buildmesh.dev/logs/buildmesh.log"
fi

# 1. Kill existing DEV instances only — leaves the stable hub running.
if pgrep -f "$BINARY" > /dev/null 2>&1; then
  echo "Stopping existing buildmesh-dev..."
  pkill -f "$BINARY" || true
  sleep 1
fi

# 2. Build the dev profile (frontend + Rust, dev overlay config)
echo "Building (dev profile)..."
npm run tauri:build:dev 2>&1 | tail -5

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
    echo "OK — Buildmesh Dev running"
    exit 0
  fi
fi

# Fallback: check process is alive
if kill -0 "$PID" 2>/dev/null; then
  echo "OK — Process alive (no log confirmation)"
  exit 0
fi

echo "ERROR: Buildmesh Dev failed to start"
exit 1
