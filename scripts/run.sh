#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

BINARY="src-tauri/target/release/buildmesh"

# Resolve the per-platform log dir the same way the Rust code does. Tauri
# uses `dirs::data_dir()` (XDG_DATA_HOME first, $HOME/.local/share fallback
# on Linux; $HOME/Library/Application Support on macOS) and the early panic
# hook re-derives the same path inline from the env vars (lib.rs:41-128).
# `%APPDATA%` is Windows-only; the .ps1 script handles that path. Without
# this, a Linux launch writes to ~/.local/share/... while the scripts look
# under ~/... and miss the panic file entirely (issue #158 follow-up).
case "$(uname)" in
  Darwin) LOG_DIR="$HOME/Library/Application Support/com.alond.buildmesh/logs" ;;
  Linux)  LOG_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/com.alond.buildmesh/logs" ;;
  *)      LOG_DIR="${APPDATA:-$HOME}/com.alond.buildmesh/logs" ;;
esac
LOG_PATH="$LOG_DIR/buildmesh.log"
# Panic-hook output files (see src-tauri/src/lib.rs:41-128 + 348-382). Two
# files, two hooks: `panic_early.log` is written by the hook installed in
# `run()` BEFORE Tauri setup; `panic.log` is the main hook's destination and
# carries the full backtrace. Same delta-capture protocol as run-dev.sh
# (issue #158) so a panic-only crash doesn't masquerade as a successful
# launch.
PANIC_LOG_PATH="$LOG_DIR/panic.log"
PANIC_EARLY_LOG_PATH="$LOG_DIR/panic_early.log"

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
# Same delta-capture for the panic-hook outputs (issue #158). Echo the
# counts to stdout so callers can parse them and slice the post-launch file.
BEFORE_PANIC_LINES=0
if [ -f "$PANIC_LOG_PATH" ]; then
  BEFORE_PANIC_LINES=$(wc -l < "$PANIC_LOG_PATH")
fi
BEFORE_PANIC_EARLY_LINES=0
if [ -f "$PANIC_EARLY_LOG_PATH" ]; then
  BEFORE_PANIC_EARLY_LINES=$(wc -l < "$PANIC_EARLY_LOG_PATH")
fi
echo "Buildmesh pre-launch line count (buildmesh.log): $BEFORE_LINES"
echo "Buildmesh pre-launch line count (panic.log): $BEFORE_PANIC_LINES"
echo "Buildmesh pre-launch line count (panic_early.log): $BEFORE_PANIC_EARLY_LINES"

# 5. Launch raw binary (never the .app bundle — it can be stale)
"$BINARY" &
PID=$!
echo "Launched PID: $PID"

# 6. Verify via log
sleep 3

# Panic-fast-fail (issue #158): same rationale as run-dev.sh — a panic-only
# crash writes to panic.log / panic_early.log but never reaches
# "started|ready" in buildmesh.log. Print the new lines verbatim so a human
# running the script directly sees the panic message + backtrace, matching
# the failure-summary shape /verify step 8 produces.
for entry in "panic.log:$BEFORE_PANIC_LINES" "panic_early.log:$BEFORE_PANIC_EARLY_LINES"; do
  fname="${entry%%:*}"
  before_count="${entry##*:}"
  path="$LOG_DIR/$fname"
  if [ -f "$path" ]; then
    cur=$(wc -l < "$path")
    if [ "$cur" -gt "$before_count" ]; then
      echo "ERROR: panic detected in $path (was $before_count lines, now $cur). Launch aborted."
      echo "----- panic entry -----"
      tail -n +$((before_count + 1)) "$path"
      echo "-----------------------"
      exit 1
    fi
  fi
done

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
