# Circuit runs 276 and 277

Investigation date: 2026-10-01. Evidence came from the stable application's
read-only SQLite ledger, rotated runtime logs, native MiniMax transcripts and
manifests, and isolated classifier CLI replays. The investigation did not rewrite
the stable run ledger or mark either implementation successful.

## Observed failures

Run 276 (issue 1935, implementer node 4724) had no captured MiniMax conversation
after its original launch. Nodes 4724 and 4725 started nearly simultaneously;
their manifests appeared at 23:15:28.653 and 23:15:28.727 on September 30. A
time-only scan could observe one manifest before the second was published, so
single-candidate matching was unsafe as well as incomplete. Node 4724's poller
eventually gave up.

After the app restarted and the implementer was freshly launched, a callback at
06:31:16.493 on October 1 captured a conversation already owned by node 4715 in
an unrelated project. Its transcript described that project's work and a pending
question. The implementation readiness check correctly rejected that transcript;
the underlying defect was the wrong identity, not the readiness rejection.

MiniMax's plugin manifest is machine-global. Every spawn baked its own numeric
node id into the callback URL. A simultaneous spawn or standalone session could
load the last writer's URL. The route trusted that address and its fill-only
database update allowed the same conversation id on two nodes.

Run 277 (issue 1934, implementer node 4725) retained the correct conversation.
Its implementation step completed and its implementation classifier became
Unverified. Context contained **348 classifier failures**, despite the checkpoint
saying classification stopped after five. The worker continued accepting
Unverified gates, and neither its report eligibility nor the transition core
enforced the exhausted budget.

The configured classifier selection was unset, meaning the independent default
Claude Code backend. A current isolated replay returned exit 1 and
`Failed to authenticate: OAuth session expired and could not be refreshed` on
stdout. Historical calls recorded only exit 1, so that exact historical error
cannot be recovered. Discarding both nonzero stdout and stderr hid the actionable
cause.

## Corrective changes

- All MiniMax spawns provision `/api/attention/mcode`. SessionStart is provisioned
  for identity capture, but its delivery is unvalidated on the installed TUI.
  Stop and PermissionRequest retain their existing behavior.
  Native conversation id and workspace select one live node. Older numeric
  callbacks use the same resolution. Unknown standalone workspaces, duplicate
  ownership and ambiguous fresh workspaces are rejected.
- Session capture fences the node snapshot and durable process generation.
  The database refuses a conversation id already owned by another node. Unsafe
  timestamp-only MiniMax discovery and restart recovery are removed.
- A five-failure budget is enforced before report inference, quiet-turn fallback,
  and state transitions. It survives restarts and changed reports. Explicit
  evidence recheck remains the recovery action after restoring the backend.
- Classifier failures retain bounded, sanitized stdout/stderr diagnostics in the
  step checkpoint. Failed inference never becomes an implementation verdict.
- Circuit classification accepts saved host-native Claude Code or native Codex
  configurations independently of implementers, reviewers and auto-naming.
  Codex honors saved model/effort and uses ephemeral read-only execution in a
  temporary directory with shell and multi-agent tools disabled. Arbitrary extra
  CLI arguments are rejected for both classifier backends; saved model and
  effort settings are applied to both.

The user selected configurable Codex classification, using `gpt-6-luna` for now.
A live isolated CLI replay using that model, `low` effort and the existing Codex
login returned `COMPLETED`, exit 0, in about seven seconds. CLI flags were checked
against installed Codex 0.159.3 and the official
[non-interactive documentation](https://developers.openai.com/codex/noninteractive).

## Regression and recovery evidence

Before the fixes, tests reproduced unlimited exhausted-budget retries, lost
authentication diagnostics, and duplicate conversation capture. After correction,
the targeted classifier suite passes, including reload of durable context, fresh
reports after exhaustion, attempt-scoped diagnostics, bounded process I/O, and
saved Codex model/effort plus final-answer selection. MiniMax coverage exercises
simultaneous workspaces, callbacks from unrelated standalone workspaces,
ambiguous ownership, known-session
resume, and preservation of user plugin handlers.

The existing corrupted run requires verified conversation recovery rather than
blindly rechecking its wrong transcript. This change rejects historical duplicate
ownership; it does not silently choose an owner or overwrite a live conversation.
Run 277 can be rechecked after selecting an authenticated classifier in an app
running the corrected code. Neither live run is evidence of a successful fixed
rerun until that rollout and recovery occur.

Workspace matching is not process authentication: an unknown standalone MiniMax
session started inside the exact same workspace as one live managed node with
no captured identity is indistinguishable using the current native hook payload.
The fix rejects unrelated workspaces and ambiguous managed ownership; it does
not claim immunity to that same-workspace case.

Final verification on Windows:

- Frontend build, ESLint and its violation fixtures passed. Integration tests:
  69 passed across 10 files.
- Full frontend unit suite with two workers: 3,585 passed, one skipped across
  257 files. Two earlier default-worker runs failed on different timing-sensitive
  tests. The recorded base passed the default-worker suite, so attribution of
  those failures remains unverified; they are not labelled pre-existing.
- Full serial Rust check: 3,971 library tests passed, 25 ignored; integration
  targets passed 8, 1 and 9 tests. The subsequent spawn-order correction passed
  73 focused MiniMax tests; the final classifier suite passed 12 tests.
- All-target Clippy passed with 35 warnings, matching the recorded base. No new
  warning was introduced. Generated TypeScript bindings did not change.
- The actual dev-profile WebView exercised real Tauri IPC: a saved Codex Luna
  configuration with low effort appeared in the classifier picker, persisted
  after reload and retained its model/effort. The after-only screenshot for this
  new option was inspected. Existing dev fixtures logged unrelated missing
  repositories; no new panic log was created. The dev runtime was stopped.
- A subsequent PR review identified defects at the classifier I/O, Claude plan,
  session identity validation and ownership seams. The follow-up changes address
  those findings. SessionStart remains provisioned but unvalidated against the
  installed TUI; stable rollout/recovery remain outstanding.

## Review follow-up verification

The review correction separates stdout and stderr limits: oversized stderr is
drained and truncated in failure diagnostics, while valid classifier stdout is
still accepted. Claude and Codex now use one resolved launch plan and reject
extra CLI arguments consistently. Archived nodes release their session ids for
reuse. Classifier errors are keyed by attempt, and the failure-count message
uses the same constant as the budget. Hook parsing is reused once per request;
MiniMax target selection checks process generation only for its unique owner.
The mcode SessionStart wait has named bounds and fake-clock deadline coverage.

Final follow-up checks:

- `cargo test --locked --lib circuit_classifier_ -- --test-threads=1`: 13 passed,
  including successful classification with oversized stderr and bounded failure
  diagnostics.
- `cargo test --locked --lib claude_classifier_uses_saved_model_and_effort_and_rejects_extra_args -- --test-threads=1`: 1 passed.
- `cargo test --locked --lib mcode_ -- --test-threads=1`: 21 passed, including
  retry deadline, archived identity reuse, and callback routing.
- `cargo test --locked --lib attention_capture_ -- --test-threads=1`: 2 passed,
  including archived-owner reuse through normal capture, live recovery, and
  suspended recovery. `cargo test --locked --lib recovery_ -- --test-threads=1`:
  37 passed, preserving active-owner and generation fences.
- `npm run check:docs` and `NODE_ENV=test npm run test:docs`: passed (131 docs,
  33 documentation tests); `npm run build`: passed.
- `rustfmt --edition 2021 --check src/services/mcode_session.rs` and
  `git diff --check`: passed. `cargo clippy --locked --all-targets` passed with
  3 library and 35 test warnings, matching the recorded base with no new
  warnings. The final Windows dev-profile build passed; the stable hub was not
  replaced.
