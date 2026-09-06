# Circuit autonomy audit

Investigated 2026-09-06 at `0aeaea6aad3c9de047fc799a36bce0f7c18bc1b5`.
Scope: explain run 73, inventory recurring failures, and define guards that let
circuits recover or finish with an actionable failure and admit subsequent work.
The initial investigation below predates the fixes; see Implemented safeguards
at the end for the subsequent implementation and verification.
The stable profile was read through SQLite `mode=ro`; no agents, preferences,
run states, or external services were changed.

## Run 73: completed source, missing observation

The stable ledger has run 73 running in circuit 6, step `await_source`, borrowing
agent 3598. There is no classifier result, evaluated report, or step error.
At the initial observation, the source row has provider `codex`, status `awaiting_input`, and a NULL
`cli_session_id`. This is a review of an existing agent, not an issue-triggered
implementation run. Circuit 6 being disabled does not explain it: manual runs
intentionally execute against disabled circuits.

The evidence chain (UTC):

| Time | Evidence |
| --- | --- |
| 13:35:29 | Node 3598 starts; durable `session_started_at` is 1788701729172. |
| 13:35:42.988635 | `buildmesh.log.3`: `codex session capture: gave up for node 3598`. |
| 13:39:14.055 | First rollout record's timestamp; metadata inside it identifies the original session start as 13:35:32.115. The record timestamp suggests delayed publication; it is not filesystem creation-time proof. |
| 13:39:26 | Run 73 and its `await_source` step start. |
| 13:51:31.503 | Parent rollout contains a final answer reporting implementation and verification completed, with changes uncommitted. |
| 13:52:31.970836 | `buildmesh.log.1`: worker reports source quiet for 60405ms and synthesizes a turn notification. |
| 13:52:31.971227 | Lifecycle log marks node 3598 awaiting input. |

The parent rollout is
`~/.codex/sessions/2026/09/06/rollout-2026-09-06T14-35-32-01a076ee-6c95-7c82-9e5f-928e9f43ad7a.jsonl`.
Its metadata matches the source worktree, identifies `source: cli`, and records
Codex 0.153.4. Two later rollouts in the same directory are subagents and must
not be used as the source report. Uncommitted changes are legitimate input to
this review blueprint, whose reviewer explicitly inspects them.

The current code explains the wedge:

1. `services/codex_session.rs::start_capture_poller` stops after delays totaling
   13.5 seconds. The runtime log confirms exhaustion for this node.
2. `coordinator/enrichment.rs::assistant_report` passes the missing session ID
   to the transcript reader; Codex lookup cannot locate a report without it.
3. `services/circuit_worker.rs::restore_run_evaluators` registers the borrowed
   source only after the run starts. Registration does not establish a turn
   boundary. Earlier source input predates registration.
4. `select_turn_report` correctly refuses a PTY tail without a live turn
   boundary, preventing stale redraws from authorizing a later gate. The
   watchdog's synthetic yield does not create such a boundary.
5. Every missing-report return is silent in the step ledger. Neither this wait
   nor the run has a general inactivity deadline.

The missing-ID transcript failure is directly reproducible from persisted
inputs. The boundary explanation follows the code and event ordering; private
in-memory evaluator state and process liveness were not directly inspected.
Startup identity recovery already exists, but `recover_suspended_node` is not
called by this live classifier path. Restarting is not an acceptable recovery
strategy for an autonomous circuit.

A later evidence recheck found the source had become `running` at
15:48:58.650234400 UTC, still with a NULL session ID, while run 73 remained
unchanged. An assertion that it was *still* awaiting input therefore failed.
The audit describes the captured earlier stall; it does not claim the source
stayed idle throughout this investigation or attribute its renewed activity
to autonomous recovery.

## Observed failure distribution

Snapshot: 71 stored runs: 38 pending, 3 running, 28 failed, 1 completed,
1 cancelled. Counts below are step rows and overlap within runs; they are not
independent failure rates. Historical failures do not prove the current source
still has their original defect.

| Evidence | Current behavior / implication | Required guard |
| --- | --- | --- |
| Run 73: no report or error | Missing identity plus unanchored tail can wait forever. | Recover identity from matching generation metadata; expose missing evidence and bound recovery. |
| Run 17: `implementation_classifier` says work remains | Unrouted WORKING is persisted and waits for a different report; no continuation is delivered. | Separate ordinary task continuation from questions/permissions; bounded continuation for an owned agent, then inactivity expiry. |
| Run 30: blocked collaborator gate | External issue author requires approval; the run remains running with a blocked step. | Preserve approval requirement; expiry or defer policy must release admission capacity without approving or executing the issue. |
| 16 `OpenPr requires a spawned agent earlier in this run` rows; 16 retry-exhaustion rows | Structural prerequisite failures can consume a retry budget without repairing ownership. Historical provenance needs a separate reproduction. | Validate reachable spawn association before action; terminate structural errors or route explicit repair instead of repeating the same lookup. |
| 11 `piloted agent node was closed` rows, including runs 70–72 | Explicit closure/deletion cancels dependent steps and fails the run. | Preserve this behavior; distinguish user closure, crash, missing identity, and recoverable disconnect in diagnostics. |
| Two lost-lineage-on-startup rows | Restart reconciliation finds missing targets and terminalizes runs. | Keep reconciliation; assert cleanup and capacity effects as well as the terminal status. |
| Run 29 dirty-worktree prerequisite; another missing-PR prerequisite | These are real unmet wrap-up conditions; existing blueprint routes corrective feedback and bounded retries. | Retain deterministic checks; retry the producer's corrective work before rechecking the PR gate. |
| One agent-error row | All agent errors become a failed run; no typed transient reason is available here. | Classify recoverable launch/service failures separately from task or authorization failures, with bounded retries. |

The stable preferences set the global pool to 6; mesh 65 admits 4 circuit runs.
The worker repeatedly logs global-pool admission denials for pending runs.
Current admission counts worst-case leases plus retained agents. Thus a queue
can be held below the mesh run cap. This evidence establishes a pool constraint,
not a reason to remove the pool: stalls and approval waits must stop retaining
capacity indefinitely. Ordinary capacity queueing should not consume execution
retry budgets.

## Additional current-code risks

- Classifier failure retries after 60 seconds, indefinitely. The 30-second
  subprocess wait timeout bounds one evaluation, not the gate's lifetime.
- `classify_step_turn` requires a live process before trying a readable report.
  A completed/exited source with valid final evidence cannot pass that check.
  Borrowed initial review can potentially accept generation-matched final
  evidence; follow-up injection still needs an explicitly resumable process.
- The worker observes gates synchronously inside its serial run loop. A
  classifier wait can delay all other runs by 30 seconds; verification can
  delay them by 120 seconds per command. These limits are not queue fairness.
  The classifier also reads stdout after waiting for exit, which deserves
  pipe/backpressure and inherited-handle tests before claiming an end-to-end
  deadline.
- Failed-run cleanup is best effort in `close_run_agents`: deletion errors
  are logged. A terminal run is no longer part of normal active-run driving.
  Add a durable cleanup retry sweep so failure cannot strand owned resources
  and keep consuming the retained-agent pool. Never delete the borrowed source.
- The graph's `RetryLimit` bounds executions of routed retry loops. It does
  not bound an AwaitAgentTurn, an unrouted WORKING/BLOCKED result, or approval.
  A timeout must not be implemented by merely adding more RetryLimit nodes.

## Implementation order and acceptance evidence

### 1. Repair report acquisition and make waits observable

At a throttled missing-report probe, reuse adapter-owned historic identity
resolution with the durable session generation and resolved working directory.
`evaluator::begin_circuit_probe` already permits another observation after its
10-second cooldown without fresh PTY output; add identity recovery at that
existing seam rather than introducing a second report polling loop.
Preserve the existing exclusion of subagents and ambiguous candidates. Persist
an identity only with a compare-and-set on the same generation/provider/path;
never overwrite a session captured concurrently or bind a regenerated node.
Extend fresh capture to tolerate delayed publication, but keep live recovery:
any finite initial poll window can still expire before the first prompt.

Return a typed observation reason for missing identity, missing transcript,
stale report, non-yielded agent, unavailable classifier, and exited process.
Persist reason changes rather than rewriting identical errors every tick.
Record next retry and last meaningful progress so the UI can explain the wait.

Acceptance: replay run 73's metadata and delayed file arrival through the
production resolver and gate seam. The parent final report becomes classifiable
without new user input or restart. Cover same-directory subagents, two ambiguous
parents, wrong directory, old generation, concurrent resume/regeneration, and
late publication. Assert that stale pre-injection reports remain rejected.

### 2. Add a durable recovery budget and inactivity deadline

Use a persisted per-step/attempt progress clock plus recovery counts. Do not
reset the deadline on polling, terminal redraws, repeated classifier errors,
or an identical WORKING report. New meaningful assistant/tool progress can
extend inactivity patience; an optional absolute execution ceiling is separate.
Paused runs need an explicit policy so a user pause is not accidental expiry.

Suggested starting policy for validation, not existing product defaults:
retry identity/transcript probes every 10 seconds; retry classifier outages
every 60 seconds up to 5 attempts; allow at most 2 autonomous continuation
turns; fail a yielded agent after 15 minutes without meaningful progress.
Long-running active tools need a different allowance from a yielded agent.
Persist budgets across restarts. Use an explicit approval expiry policy rather
than silently applying an execution timeout to every human-controlled gate.

Feed expiry through the pure stepper and its existing failure/cascade path.
Commit terminal state before cleanup and recheck durable state before effects.
Track cleanup independently until it succeeds. Admit the next eligible pending
run only within real remaining process capacity.

Acceptance: fake-clock tests cover outage recovery, deadline boundary, repeated
noise, useful progress, restart, pause/resume, concurrent cancellation, stale
completion, blocked approval, and cleanup failure/retry. Assert durable reason,
run state, agent ownership, released leases, and subsequent queue admission.

### 3. Continue safe work without answering permission prompts

The existing three-way classifier intentionally combines permission/questions
with intermediate work. WORKING alone is insufficient authority for an
automatic reply. Use lifecycle evidence and a more precise report decision to
distinguish a yielded progress summary from permission, credentials, an explicit
question, or a live background task. Continue only existing authorized work,
with a bounded prompt and an exactly-once delivery/reconciliation contract.
Borrowed review sources must not receive unsolicited implementation prompts
merely because their initial report is missing.

Acceptance: an owned agent that says it will continue but has yielded receives
one continuation and advances. Permission/question/background cases do not.
Restart and failed delivery cannot duplicate a committed continuation.

### 4. Isolate external work and classify retry causes

Move long classifier/verification observations off the serial scheduler, with
bounded in-flight work and results keyed to run, node, and attempt. Reject
results after cancellation, timeout, pause as appropriate, or a new attempt.
Use reasoned retry policy: temporary provider/network failures get backoff;
missing lineage gets structural failure; dirty work gets the existing corrective
prompt; approval gets expiry/defer without bypass.

Acceptance: one stuck provider does not prevent another run's notification,
timeout, cleanup, or admission. Test pipe saturation and process-tree cleanup
at the actual subprocess boundary. Replay missing-lineage and dirty-worktree
cases separately; a repeated structural error must not launch futile retries.

## Verification limits

Evidence here consists of read-only production ledger queries, bounded local
rollout inspection, rotated runtime logs, and inspection of owning modules at
the recorded base. No code changes, application rebuild, live classifier call,
or autonomous recovery was performed. Existing classifier-recovery tests and
their prior results are documented in `circuit-classifier-recovery.md`; those
results are not a new test run for this audit. The proposed guards require the
behavioral tests above before implementation can be called complete.
`npm run check:agent -- --base 0aeaea6aad3c9de047fc799a36bce0f7c18bc1b5`
passed. No application test suite was run for this documentation-only change.
Independent Standards and Spec reviews found no blocking findings. The Spec
review's clarification to reuse the existing periodic probe was incorporated.

## Implemented safeguards

The follow-up implementation in this worktree addresses the confirmed causes:

- Codex startup capture continues at a slower cadence for roughly five minutes.
  It rejects ambiguous parent sessions instead of choosing the newest one.
  Waiting circuit probes also recover missed identities through the existing
  provider adapter using the saved launch generation and working directory.
  Conditional database writes reject replaced nodes, changed paths/providers,
  duplicate identity claims, and suspended nodes.
- Completed agents can supply readable reports even after their process exits.
  PTY fallback still requires a live process and a recorded turn boundary.
- Wait reasons and inactivity clocks persist per step/attempt. Defaults are
  15 minutes after yield, two hours without a new report while running, and one
  hour for approval. Changes between active and yielded modes start a fresh
  allowance. New report revisions refresh patience; polls and redraws do not.
  Pausing suppresses expiry and explicit resume grants a fresh wait allowance.
- Classifier outages end the run after five attempts. A circuit-specific
  CONTINUE verdict distinguishes an explicit ordinary next step from questions,
  permissions, and background work. At most two continuations are delivered
  per gate attempt, only to owned agents. Existing report and attempt
  deduplication remains durable without an in-context application-version tag.
- Continuations persist a claimed delivery before external effects and settle
  it through a stepper event only after Enter acknowledgement. A legacy pending
  delivery is claimed on replay; a claimed delivery is uncertain after restart
  and waits for fresh evidence or expiry rather than risking duplicate input.
  Process/turn stamps and report revisions are checked again before delivery.
  The process registry also versions accepted input under its writer lock:
  an existing partial draft disallows continuation, and newer input prevents
  both prompt staging and the separate Enter submission/retries.
- Missing OpenPr ownership fails the run before entering the action retry loop.
  Existing corrective wrap-up and review retry paths remain in place.
- Failure/cancellation atomically records cleanup intent and releases run
  leases. Failed cleanup retries every 30 seconds and survives restart and
  ledger retention. Borrowed sources and agents referenced by another active
  run are excluded. Historic terminal runs are not retroactively opted in.
- Classifier subprocess stdin/stdout are drained concurrently with bounded
  output and a deadline, avoiding pipe-buffer and inherited-output-handle hangs.
  Windows descendants are contained by the existing JobHandle mechanism when
  available.

These are bounded recovery changes, not an asynchronous scheduler rewrite:
individual classifier and verification operations still occupy the serial
worker within their operation limits. A running tool that publishes no new
assistant report for two hours can exhaust the active inactivity allowance.

Regression coverage includes delayed Codex metadata, subagent exclusion,
ambiguous sessions, conditional identity claims, pause/restart/stale attempts,
active-to-yielded transitions, continuation replay and stale delivery, approval
expiry, classifier retry limits, atomic lease release, cleanup ownership,
retention, and actual subprocess backpressure/timeouts.

A temporary read-only test loaded agent 3598's launch generation and directory
from the stable SQLite database, resolved the correct parent ID through the
production Codex resolver, and read its report through the production transcript
reader. It passed and was removed; the portable delayed-report fixture remains.
No live run state, session identity, agent process, or stable app installation
was modified. The application must be rebuilt and restarted to use these fixes.

Final Windows verification:
- `scripts/check.ps1 rust -SerialRust`: passed; 2,911 library tests and 18
  integration tests executed, 14 library tests and one doctest ignored; six
  agent infrastructure tests passed.
- Focused circuit regression suite: 137 tests passed, including the actual
  process-registry input queue race test. The ambiguous-parent regression
  failed before the discovery fix and passed afterward.
- `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets`:
  exit 0; the 45 warning records match the pre-remediation baseline, so this
  change adds no new warnings.
- `npm run check:agent -- --base 0aeaea6aad3c9de047fc799a36bce0f7c18bc1b5`
  and `git diff --check`: passed; no generated binding changes.
- Mobile asset build passed. No end-to-end run in the rebuilt desktop app was
  performed; the live evidence above exercises read-only recovery/report lookup.
