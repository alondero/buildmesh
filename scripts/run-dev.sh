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

# Resolve the per-platform log dir the same way the Rust code does. Tauri
# uses `dirs::data_dir()` (XDG_DATA_HOME first, $HOME/.local/share fallback
# on Linux; $HOME/Library/Application Support on macOS) and the early panic
# hook re-derives the same path inline from the env vars (lib.rs:41-128).
# `%APPDATA%` is Windows-only; the .ps1 script handles that path. Without
# this, a Linux launch writes to ~/.local/share/... while the scripts look
# under ~/... and miss the panic file entirely (issue #158 follow-up).
case "$(uname)" in
  Darwin) LOG_DIR="$HOME/Library/Application Support/com.alond.buildmesh.dev/logs" ;;
  Linux)  LOG_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/com.alond.buildmesh.dev/logs" ;;
  *)      LOG_DIR="${APPDATA:-$HOME}/com.alond.buildmesh.dev/logs" ;;
esac
LOG_PATH="$LOG_DIR/buildmesh.log"
# Panic-hook output files (see src-tauri/src/lib.rs:41-128 + 348-382). Two
# files, two hooks: `panic_early.log` is written by the hook installed in
# `run()` BEFORE Tauri setup, so it captures panics during Tauri-init that
# the main hook can't; `panic.log` is the main hook's destination and carries
# the full backtrace. /verify's log-scan tier slices these by pre-launch line
# count to detect a panic-only crash that produces no `ERROR` line in
# buildmesh.log (issue #158).
PANIC_LOG_PATH="$LOG_DIR/panic.log"
PANIC_EARLY_LOG_PATH="$LOG_DIR/panic_early.log"

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
# Same delta-capture for the panic-hook outputs (issue #158). Echo the
# counts to stdout so /verify can parse them and slice the post-launch file.
BEFORE_PANIC_LINES=0
if [ -f "$PANIC_LOG_PATH" ]; then
  BEFORE_PANIC_LINES=$(wc -l < "$PANIC_LOG_PATH")
fi
BEFORE_PANIC_EARLY_LINES=0
if [ -f "$PANIC_EARLY_LOG_PATH" ]; then
  BEFORE_PANIC_EARLY_LINES=$(wc -l < "$PANIC_EARLY_LOG_PATH")
fi
echo "Buildmesh Dev pre-launch line count (buildmesh.log): $BEFORE_LINES"
echo "Buildmesh Dev pre-launch line count (panic.log): $BEFORE_PANIC_LINES"
echo "Buildmesh Dev pre-launch line count (panic_early.log): $BEFORE_PANIC_EARLY_LINES"

# 5. Launch raw binary (never the .app bundle — it can be stale)
"$BINARY" &
PID=$!
echo "Launched PID: $PID"

# 6. Verify via log
sleep 3

# Panic-fast-fail (issue #158): a panic-only crash writes to panic.log /
# panic_early.log but never reaches "started|ready" in buildmesh.log, so
# surface that condition before the normal "started|ready" check. Print the
# new lines verbatim so a human running the script directly sees the panic
# message + backtrace, matching the failure-summary shape /verify step 8
# produces (skill.md `### panic.log + panic_early.log slices`).
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
