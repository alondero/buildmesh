#!/usr/bin/env bash
# Builds and launches the DEV profile (buildmesh-dev) so it runs side-by-side
# with the stable hub (buildmesh) without interrupting its agents. This is what
# /use and /verify call. It only ever touches buildmesh-dev — never the hub.
set -euo pipefail

DETACH=false
TEST_BRIDGE=false
READINESS_TIMEOUT_SECONDS=30
while [ "$#" -gt 0 ]; do
  case "$1" in
    --detach) DETACH=true; shift ;;
    --test-bridge) TEST_BRIDGE=true; shift ;;
    --readiness-timeout-seconds)
      READINESS_TIMEOUT_SECONDS="${2:-}"
      if ! [[ "$READINESS_TIMEOUT_SECONDS" =~ ^[0-9]+$ ]] || [ "$READINESS_TIMEOUT_SECONDS" -lt 1 ] || [ "$READINESS_TIMEOUT_SECONDS" -gt 300 ]; then
        echo 'ERROR: --readiness-timeout-seconds must be between 1 and 300'
        exit 1
      fi
      shift 2 ;;
    *) echo "ERROR: unknown option $1"; exit 1 ;;
  esac
done

cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"

# --- Instance identity -----------------------------------------------------
# Two worktrees can each verify their own build, so "the dev instance" is a
# per-worktree thing, not a per-machine one (issue #2345). A linked worktree
# therefore gets its own app-data profile and its own port offset:
#
#   * Its own profile, because exactly one process may own a profile
#     (src-tauri/src/instance_guard.rs). Sharing the canonical one would make
#     the second launch exit as a duplicate instead of running.
#   * Its own offset, because the ports come from the bundle identifier
#     (`*.dev` = +1000), so two dev builds would both bind 2991-2994.
#
# The main checkout is deliberately unchanged: it keeps the canonical
# `com.alond.buildmesh.dev` profile and the identifier's 1000 offset, so an
# ordinary dev launch behaves exactly as before. A linked worktree is the one
# whose `.git` is a *file* rather than a directory: `git worktree add` writes
# `gitdir: ...` into it. Anything else (a plain checkout, an exported copy, a
# test fixture) is the main checkout.
# The leaf alone is not unique (`.worktrees/<name>` and `.claude/worktrees/<name>`
# are both real worktree layouts), so pair it with a hash of the full path,
# lower-cased so two spellings of one path resolve to one profile directory.
worktree_slug() {
  local leaf safe lower digest
  leaf="$(basename "$REPO_ROOT")"
  safe="$(printf '%s' "$leaf" | tr -c 'A-Za-z0-9._-' '-')"
  [ -n "$safe" ] || safe="worktree"
  safe="$(printf '%.48s' "$safe")"
  lower="$(printf '%s' "$REPO_ROOT" | tr '[:upper:]' '[:lower:]')"
  if command -v shasum > /dev/null 2>&1; then
    digest="$(printf '%s' "$lower" | shasum -a 256 | cut -c1-8)"
  elif command -v sha256sum > /dev/null 2>&1; then
    digest="$(printf '%s' "$lower" | sha256sum | cut -c1-8)"
  else
    # cksum is in POSIX; its first field is all that is needed to tell a
    # handful of worktrees apart.
    digest="$(printf '%s' "$lower" | cksum | cut -d ' ' -f1 | cut -c1-8)"
  fi
  printf '%s-%s' "$safe" "$digest"
}

# A port is busy when something answers on loopback. Bash's /dev/tcp is the
# only probe available without extra tools, and the subshell closes the fd.
port_in_use() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2> /dev/null
}

# Ports the app binds at a given offset: the dev test bridge (1991) and the
# HTTP/WebSocket range (1992-1994). These must match
# src-tauri/src/http/state.rs (HTTP_PORT_START/END) and commands/test.rs
# (TEST_SERVER_PORT).
port_range_free() {
  local offset="$1" base
  for base in 1991 1992 1993 1994; do
    if port_in_use "$((base + offset))"; then return 1; fi
  done
  return 0
}

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
export CARGO_TARGET_DIR="$REPO_ROOT/src-tauri/target/release-dev"
BINARY="$CARGO_TARGET_DIR/release/buildmesh-dev"

# Resolve the per-platform data root the same way the Rust code does. Tauri
# uses `dirs::data_dir()` (XDG_DATA_HOME first, $HOME/.local/share fallback
# on Linux; $HOME/Library/Application Support on macOS) and the early panic
# hook re-derives the same path inline from the env vars (lib.rs:41-128).
# `%APPDATA%` is Windows-only; the .ps1 script handles that path.
case "$(uname)" in
  Darwin) DATA_ROOT="$HOME/Library/Application Support" ;;
  Linux)  DATA_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}" ;;
  *)      DATA_ROOT="${APPDATA:-$HOME}" ;;
esac
DEV_ROOT="$DATA_ROOT/com.alond.buildmesh.dev"
WORKTREE_SLUG=""
if [ -f "$REPO_ROOT/.git" ]; then
  WORKTREE_SLUG="$(worktree_slug)"
fi
# An explicit BUILDMESH_PROFILE_DIR wins: a caller that pinned a profile (or a
# harness fixture) gets exactly that, and this script neither derives nor
# unsets it. Otherwise a linked worktree gets a profile of its own.
PROFILE_DIR="${BUILDMESH_PROFILE_DIR:-$DEV_ROOT}"
if [ -z "${BUILDMESH_PROFILE_DIR:-}" ] && [ -n "$WORKTREE_SLUG" ]; then
  PROFILE_DIR="$DEV_ROOT/worktrees/$WORKTREE_SLUG"
fi
# Kept so the launch can hand the caller's own values back afterwards: these
# variables are exported for the launch only, and unsetting a value the caller
# set would be the script editing its caller's environment.
CALLER_PROFILE_DIR="${BUILDMESH_PROFILE_DIR:-}"
CALLER_PORT_OFFSET="${BUILDMESH_PORT_OFFSET:-}"
# The app writes its logs under the profile it actually opened, so the
# readiness and panic checks below have to read that directory, not the
# canonical one, or a worktree's launch would never see its own startup lines.
LOG_DIR="$PROFILE_DIR/logs"
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
# The profile has to exist before the launch: a first run in a worktree has no
# log file yet, and the readiness check can only compare against a baseline it
# can read from where the app will actually write.
mkdir -p "$LOG_DIR"

INSTANCE_RECORD="$REPO_ROOT/.tmp/dev-instance.json"
recorded_pid=""
recorded_offset=""
if [ -f "$INSTANCE_RECORD" ]; then
  # Read with sed rather than a JSON parser: both values are written by this
  # script, and a damaged record is a cache of a PID, not state anything else
  # depends on. Anything unparseable leaves both fields empty.
  recorded_pid="$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$INSTANCE_RECORD" | head -1)"
  recorded_offset="$(sed -n 's/.*"portOffset"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$INSTANCE_RECORD" | head -1)"
fi

# 1. Stop the DEV instance this worktree launched, and only that one.
#
#    Process name alone is not an identity: every worktree's dev build is
#    called `buildmesh-dev` at the same relative path, so `pkill -f` on it
#    took down other sessions' instances and their agents (issue #2345).
#    Candidates come from the recorded PID and from a path search, and both
#    are confirmed against this worktree's own absolute executable path.
is_our_instance() {
  [ -n "${1:-}" ] || return 1
  kill -0 "$1" 2> /dev/null || return 1
  ps -p "$1" -o command= 2> /dev/null | grep -qF -- "$BINARY"
}

if [ -n "$recorded_pid" ] && kill -0 "$recorded_pid" 2> /dev/null &&
  ! is_our_instance "$recorded_pid"; then
  echo "Left PID $recorded_pid alone: recorded for this worktree but not running $BINARY"
fi
STOPPED=""
for pid in $(pgrep -f "$BINARY" 2> /dev/null || true); do
  if is_our_instance "$pid"; then
    STOPPED="$STOPPED $pid"
    kill "$pid" 2> /dev/null || true
  fi
done
if [ -n "$STOPPED" ]; then
  echo "Stopping this worktree's buildmesh-dev (PID$STOPPED)..."
  sleep 1
fi
# The record is consumed here. Remove it even when nothing matched, so a stale
# PID cannot be mistaken for a live instance on the next run.
rm -f "$INSTANCE_RECORD"

echo "Dev profile: $PROFILE_DIR"

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
BEFORE_FIRST_LINE=""
if [ -f "$LOG_PATH" ]; then
  BEFORE_LINES=$(wc -l < "$LOG_PATH")
  BEFORE_FIRST_LINE=$(head -n 1 "$LOG_PATH")
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

# Resolve this launch's ports, as late as possible so a long build cannot let
# another worktree take the range first. An explicit BUILDMESH_PORT_OFFSET
# passes through untouched and is never probed; otherwise only a linked worktree
# probes, preferring the range it used last time so its ports stay put across
# restarts.
PORT_OFFSET=""
if [ -n "${BUILDMESH_PORT_OFFSET:-}" ]; then
  echo "Dev ports: offset $BUILDMESH_PORT_OFFSET (from BUILDMESH_PORT_OFFSET)"
elif [ -n "$WORKTREE_SLUG" ]; then
  CANDIDATES=()
  if [ -n "$recorded_offset" ]; then CANDIDATES+=("$recorded_offset"); fi
  for slot in $(seq 0 80); do CANDIDATES+=("$((1000 + 100 * slot))"); done
  for candidate in "${CANDIDATES[@]}"; do
    if port_range_free "$candidate"; then
      PORT_OFFSET="$candidate"
      break
    fi
  done
  if [ -z "$PORT_OFFSET" ]; then
    echo "ERROR: no free dev port range (1991-2994 offsets 1000-9000) for this worktree"
    exit 1
  fi
  echo "Dev ports: offset $PORT_OFFSET (test bridge $((1991 + PORT_OFFSET)), HTTP $((1992 + PORT_OFFSET))-$((1994 + PORT_OFFSET)))"
else
  echo "Dev ports: identifier offset 1000 (test bridge 2991, HTTP 2992-2994)"
fi

# 5. Launch raw binary (never the .app bundle — it can be stale).
# RUST_BACKTRACE=1 enables std::backtrace::Backtrace::capture() in the panic
# hook (lib.rs setup()) so the dev profile's panic.log gets real frames
# instead of the "disabled backtrace" placeholder. Issue #152.
export RUST_BACKTRACE=1
# Opt-in development test bridge. Keep in sync with run-dev.ps1 -TestBridge.
# The token is not printed. Without --test-bridge a stale token file is removed
# so another process cannot authenticate against an app that is not listening.
TOKEN_FILE=".tmp/test-bridge-token"
if "$TEST_BRIDGE"; then
  TOKEN=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
  mkdir -p .tmp
  printf '%s' "$TOKEN" > "$TOKEN_FILE"
  export BUILDMESH_TEST_BRIDGE=1
  export BUILDMESH_TEST_BRIDGE_TOKEN="$TOKEN"
  echo "Test bridge enabled; credential written to .tmp/test-bridge-token"
else
  rm -f "$TOKEN_FILE"
fi
# Issue #2136: /use and /verify launch the dev profile from inside an agent's
# Claude Code session, and that session exports its own markers into every
# process it spawns. Handed on to an agent Buildmesh spawns,
# CLAUDE_CODE_CHILD_SESSION makes Claude Code print "Transcript saving is off",
# write no .jsonl, and park every transcript reader downstream. The app scrubs
# these per spawn (agent::spawn_environment), so this is defence in depth for
# the paths the scrub cannot reach; clearing here makes the dev profile behave
# like a user's own launch. Same launch-only scope as RUST_BACKTRACE above.
# Keep in sync with run-dev.ps1.
CLAUDE_SESSION_MARKERS=(
  CLAUDECODE
  CLAUDE_CODE_CHILD_SESSION
  CLAUDE_CODE_SESSION_ID
  CLAUDE_CODE_SESSION_ATTENDED
  CLAUDE_CODE_BRIDGE_SESSION_ID
  CLAUDE_CODE_ENTRYPOINT
  CLAUDE_CODE_EXECPATH
  CLAUDE_CODE_MESSAGING_TOKEN
  CLAUDE_CODE_MESSAGING_SOCKET
  CLAUDE_PID
  CLAUDE_EFFORT
)
CLEARED=""
for marker in "${CLAUDE_SESSION_MARKERS[@]}"; do
  if [ -n "${!marker:-}" ]; then
    unset "$marker"
    CLEARED="$CLEARED $marker"
  fi
done
if [ -n "$CLEARED" ]; then
  echo "Cleared Claude Code session markers for this launch:$CLEARED"
fi
# Same launch-only scope as RUST_BACKTRACE for the two instance-identity
# variables: the app reads them at startup and nothing else in this shell
# should. Exported BEFORE the fork, because a backgrounded launch can exec
# before the next line runs.
if [ -z "$CALLER_PROFILE_DIR" ]; then export BUILDMESH_PROFILE_DIR="$PROFILE_DIR"; fi
if [ -n "$PORT_OFFSET" ]; then export BUILDMESH_PORT_OFFSET="$PORT_OFFSET"; fi
if "$DETACH"; then
  stop_launch() {
    kill "$PID" 2>/dev/null || true
    STOP_DEADLINE=$((SECONDS + 3))
    while kill -0 "$PID" 2>/dev/null && [ "$SECONDS" -lt "$STOP_DEADLINE" ]; do sleep 0.1; done
    kill -KILL "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
    # The instance the record names no longer exists.
    rm -f "$INSTANCE_RECORD"
  }
  mkdir -p .harness/logs
  # nohup plus redirected handles lets the invoking shell exit independently.
  nohup "$BINARY" </dev/null >>.harness/logs/dev-app.stdout.log 2>>.harness/logs/dev-app.stderr.log &
else
  "$BINARY" &
fi
PID=$!
unset BUILDMESH_TEST_BRIDGE
unset BUILDMESH_TEST_BRIDGE_TOKEN
# Hand the caller its own values back; clear only what this script set.
if [ -z "$CALLER_PROFILE_DIR" ]; then
  unset BUILDMESH_PROFILE_DIR
else
  export BUILDMESH_PROFILE_DIR="$CALLER_PROFILE_DIR"
fi
if [ -z "$CALLER_PORT_OFFSET" ]; then
  unset BUILDMESH_PORT_OFFSET
else
  export BUILDMESH_PORT_OFFSET="$CALLER_PORT_OFFSET"
fi
echo "Launched PID: $PID"
echo "Log: $LOG_PATH"
# Record the instance before waiting on it, so the next run from this worktree
# stops exactly this process. Written early on purpose: a consumer that stops
# reading stdout early can cut the pipe underneath the shell, and the PID must
# not depend on reaching the end.
mkdir -p "$(dirname "$INSTANCE_RECORD")"
printf '{\n  "pid": %s,\n  "binary": "%s",\n  "profileDir": "%s",\n  "portOffset": %s,\n  "launchedAt": "%s"\n}\n' \
  "$PID" "$BINARY" "$PROFILE_DIR" "${PORT_OFFSET:-null}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  > "$INSTANCE_RECORD"

if "$DETACH"; then
  # Compilation is outside the readiness budget, just as in run-dev.ps1.
  DEADLINE=$((SECONDS + READINESS_TIMEOUT_SECONDS))
  while [ "$SECONDS" -lt "$DEADLINE" ]; do
    for entry in "panic.log:$BEFORE_PANIC_LINES" "panic_early.log:$BEFORE_PANIC_EARLY_LINES"; do
      path="$LOG_DIR/${entry%%:*}"
      before_count="${entry##*:}"
      if [ -f "$path" ] && [ "$(wc -l < "$path")" -gt "$before_count" ]; then
        echo "ERROR: panic detected in $path. Launch aborted."
        tail -n +$((before_count + 1)) "$path"
        stop_launch
        exit 1
      fi
    done
    if ! kill -0 "$PID" 2>/dev/null; then
      echo "ERROR: Buildmesh Dev exited before readiness (PID $PID)"
      rm -f "$INSTANCE_RECORD"
      exit 1
    fi
    if [ -f "$LOG_PATH" ]; then
      SKIP_LINES=$BEFORE_LINES
      if [ "$(wc -l < "$LOG_PATH")" -lt "$SKIP_LINES" ] || [ "$(head -n 1 "$LOG_PATH")" != "$BEFORE_FIRST_LINE" ]; then SKIP_LINES=0; fi
      if tail -n +$((SKIP_LINES + 1)) "$LOG_PATH" | grep -qi 'started\|ready'; then
        echo "OK - Buildmesh Dev running detached (PID $PID)"
        exit 0
      fi
    fi
    sleep 0.1
  done
  stop_launch
  echo "ERROR: Buildmesh Dev readiness timed out after ${READINESS_TIMEOUT_SECONDS}s; see $LOG_PATH"
  exit 1
fi

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
    echo "OK - Buildmesh Dev running"
    exit 0
  fi
fi

# Fallback: check process is alive
if kill -0 "$PID" 2>/dev/null; then
  echo "OK - Process alive (no log confirmation)"
  exit 0
fi

echo "ERROR: Buildmesh Dev failed to start"
rm -f "$INSTANCE_RECORD"
exit 1
