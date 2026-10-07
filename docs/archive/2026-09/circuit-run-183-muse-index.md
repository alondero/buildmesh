# Circuit run 183: a Muse source invisible to identity capture

Investigated on 2026-09-21 from `da416a06`. Evidence came from read-only
inspection of the stable profile's SQLite ledger (`mode=ro`), `buildmesh.log`,
Muse's own `session-index.db` and per-session logs on disk. The stable
process, its ledger, and the reviewed agent's worktree were not changed.

## What run 183 did

Circuit 6 ("Review agent 3534") borrowed agent 4308
(`gh1816-fixcircuit-gate-the-reviewer-provider-on-at`, provider `muse`, issue
#1816). The run was minted with the review preset's explicit override
(`source.review_allow_unobserved = "1"`) and reached `await_source`.

All times below are UTC.

- 18:47:21: node 4308 is accepted and spawned (`provider=Muse`,
  `spawn_timing: session=4308 TOTAL elapsed=4060ms`).
- 18:47:29: Muse writes the session's `runtime.session.metadata` frame
  (`sessions/2026/09/21/01a0c54b-…/session.jsonl`, workspace
  `F:\src\buildmesh\.claude\worktrees\gh1816-…`).
- 18:49:48: run 183 starts; `await_source` begins waiting on node 4308.
- 18:51:26.592: `buildmesh.log` records
  `muse session capture: node 4308 produced no session identity within 240s;
  circuit waits on it will fail fast as unobserved (#1791)`.
- 19:04:50: `await_source` fails — 15 minutes after the run started — with
  `agent produced no session identity or report within 15 minutes — inspect the
  agent terminal`.

The ledger records the state, not just the message:

```
agent_nodes.cli_session_id        = (empty)
node.await_source.wait.observed   = 0
node.await_source.wait.timeout_ms = 7200000
node.await_source.status          = failed
```

Node 4308 was still `running` and its Muse session log had 4,731 lines in the
normal shape. Nothing was wrong with the agent.

## Cause

**A live Muse session has no `session-index.db` row, and every Muse lookup read
only that index.**

Muse publishes `session-index.db` asynchronously: a session gains its row when a
*later* Muse process starts (or the session closes), not while it runs. At the
time of the failure the index held 25 rows whose newest `created_at_us`
(1790006887651971, ≈16:08) predated node 4308 by ~39 minutes, and no row for
`01a0c54b-…` exists even now. Session `01a0c54b-…` was a healthy, live
conversation the whole time.

Three code paths resolved through the index alone, so the node was invisible end
to end:

1. **Identity capture** — `agent::provider::adapters::muse::find_session`
   iterated index rows, read each log's metadata frame, and matched on
   workspace + timestamp. With no row, it returned `None` for the whole
   `MUSE_CAPTURE_RETRY_MS` window, so `cli_session_id` stayed empty.
2. **Turn watcher** — `services::muse_watcher` resolved the log path through the
   same index, so no `run/terminal` was ever tailed.
3. **Transcript reader** — `services::transcript_reader::adapters::muse`
   resolved the log path by session id through the index, so no report could be
   read.

With no identity and no report, `observe_waits` computed `observed = false`
(`cli_session_id` empty *and* no report revision) and the #1791 fast fail ended
the wait after 15 minutes. The #1792 readiness gate and the #1791 watchdog did
exactly what they were built to do; the defect was that a live Muse node could
never become observable in the first place. The same gap explains why a Muse
node produces no turn signal or digest until a later Muse process happens to
flush the index.

The JSONL metadata frame on disk carries the same identity the index would —
session id, workspace, and timestamp — so the index is not the only possible
source. The earlier learning note
(`docs/learning/windows-wsl-harness-interop.md`) had already recorded that the
index *fields* can be NULL and the JSONL is authoritative for them; it did not
cover the row being **absent**.

## Changes

- New `src-tauri/src/services/muse_sessions.rs` owns index-independent Muse
  discovery. `data_root` resolves the Muse data root env-aware
  (native/WSL/Windows interop); `session_metadata` reads the identity frame
  through either a direct record or a retained-frame envelope;
  `workspace_candidates` unions indexed rows with a bounded scan of
  `sessions/YYYY/MM/DD/<uuid>/session.jsonl` around the launch anchor, leaving
  `select_recovery_identity` to disambiguate; `log_path` prefers the index row
  and otherwise scans the tree for the session's directory.
- `find_session` (identity capture) now delegates to `workspace_candidates` +
  `select_recovery_identity`. It resolves the root from the database path, so
  the existing index contract test is unchanged.
- `muse_watcher::session_log_path_in` and
  `transcript_reader::adapters::muse::muse_locator_in` delegate to
  `muse_sessions::log_path`, so the watcher and the report reader recover the
  same live session the index has not published.
- The scan is bounded: at most the three day directories around the launch
  anchor for workspace matching, and a capped newest-first walk of day
  directories for by-id resolution. The index remains the fast path; the disk
  scan runs only on a miss for by-id, and adds candidates for identity recovery.
  Each database connection is dropped before any file is read (no filesystem
  work while a connection is held).

## Limits

- The fallback reads Muse's on-disk layout, which is an observed version-specific
  contract, not a published API (Muse Code 1.3.0). A
  `SESSION_JSONL`/date-directory layout change would simply fall back to the
  index-only behaviour that failed here; the discovery
  tests pin the current layout.
- Day directories are named in UTC; the scan covers the anchor's day and its
  neighbours, so a host-timezone disagreement is absorbed, but a session log
  written to a fourth day would not be found by the workspace scan.
- This does not change Muse's asynchronous index. A node captured by the disk
  fallback still carries a `cli_session_id` that Muse has not indexed; resume
  and recovery read the log directly, so they no longer depend on the row.
- Verification is source-level plus synthetic filesystem fixtures. No live
  circuit run was started, and the failing stable process was not restarted;
  a rebuilt binary is required before the change affects running agents.

## Verification

Windows, worktree `noted-standing-blossom`, base `da416a06`.

- `cargo test --locked --lib muse -- --test-threads=1`: **82 passed, 0 failed,
  4 ignored** (4 live tests that need an installed/authenticated Muse). This
  includes the four new regressions and every pre-existing Muse test.
- The adapter regression is red without the fix by construction: it builds a
  root with no `session-index.db` and asserts the identity comes from the tree,
  while the previous `find_session` opened the index and returned `None` when it
  was missing. The watcher and reader regressions likewise call the previous
  index-only lookups with no index present.
- `scripts\check.ps1 rust -SerialRust`: **3576 passed, 2 failed, 21 ignored**.
  Both failures reproduce identically at the recorded base `da416a06` with the
  change stashed — `commands::agent_tests::tests::agent_pty_gets_colour_capable_env`
  and `sandbox::spawn::tests::curated_env_prepends_git_and_redirects_temp` — and
  are caused by the coding-agent shell leaking `TERM=vscode` and
  `TEMP=…\AppData\Local\Temp` into the process environment. They are pre-existing
  and unrelated to this change.
- `scripts\check.ps1 docs`: passed — `npm run test:docs` and `npm run check:docs`
  (117 Markdown files, links/anchors and release-note checks green).
- `cargo clippy --locked --lib --tests`: only pre-existing warnings outside the
  touched files.
- `npm run check:agent -- --base HEAD`: passed.
