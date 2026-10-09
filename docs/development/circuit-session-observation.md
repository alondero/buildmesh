# Circuit session observation and autonomous supervision

Status: current

Audience: circuit and harness maintainers. This document is an architecture
assessment and implementation contract dated September 26, 2026. Recorded
verification is below; remaining acceptance requirements are not claims of
executed tests.

Buildmesh should let an operator supervise many sessions by surfacing actionable
exceptions while ordinary work progresses independently. The existing pure
stepper, transactional ledger, identity fences and effect journal support that
direction. Reliability requires correcting the observation and scheduling
boundaries, rather than replacing the workflow with another model judgement.

## Diagnosis

Historical failures repeatedly made observation depend on the state it needed
to establish. [Run 86](../archive/2026-09/circuit-run-86-observer-recovery.md) needed a watcher to
publish yield, but watcher recovery waited for yield. [Run 122](../archive/2026-09/review-circuit-handoff-audit.md)
had native completion while its display remained Running. [September 26 stalls](../archive/2026-09/circuit-failures-2026-09-26.md)
required ownership coverage unavailable from installed harnesses and stopped
observing Unverified steps. [Run 104](../archive/2026-09/circuit-run-104-watchdog.md) shows the
opposite error: quiet background work was treated as a finished review.

Run 239 exposes another broken boundary: terminal focus/cursor protocol traffic
can contaminate input tracking, preventing report binding. Repeated successful
classification cannot repair an unusable evidence binding. The particular packet
that affected the historical run was not retained, so the protocol defect is a
reproduced explanation rather than a proven packet-level reconstruction.

## Observation contract

Keep these independent facts and decisions:

| Evidence or decision | What it establishes |
|---|---|
| Foreground lifecycle | A particular native turn started, yielded or ended |
| Owned work | Known children/background tasks and their observed states; incomplete coverage remains explicit |
| Human request | An identified question or permission request, distinct from generic AwaitingInput status |
| Report publication | A stable report associated with the session, input and report revision |
| Semantic interpretation | What that report says about progress, task outcome or review findings |
| Workflow authorization | Whether the current gate's evidence and policy allow an effect |

A native foreground completion is not proof of task correctness or absence of
background work. A guarded report-based handoff can advance a circuit without
claiming complete native ownership coverage. Known unfinished work, evidence
conflicts and actual requests remain blockers. Review approval and external
merge requirements remain separate.

The shared `circuit::report_admission` policy reads the last persisted harness
lifecycle report as negative evidence: explicit questions and permissions block
report handoff, and BackgroundRunning blocks it as known outstanding work.
Generic InputRequired is not a structured request. The positive status projection
still rejects snapshots whose timestamp or status no longer matches the node row,
but process-only status writes do not release a negative lifecycle veto. Only a
new settled lifecycle report can replace that report. WorkResumed and
TurnCompleted are settled reports that release the veto; ProcessIdle,
ProcessRunning and other process projections do not. An unreadable snapshot is
treated as unavailable lifecycle evidence and fails closed. Report preflight,
transactional classification commit, watchdog recovery and operator completion attestation all
apply this policy; the commit paths re-read under the database writer. Child-work
receipts remain writable so they can resolve separate durable child evidence,
but they do not clear a lifecycle veto. The last harness lifecycle report also
survives a Buildmesh process restart; a restart or process reconciliation alone
does not prove that a request or background task was settled.

A missing agent row or unreadable lifecycle report fails closed as
`LifecycleEvidenceUnavailable`: it cannot prove that the last harness request
was settled. The Circuit evidence view names the missing or inconsistent
lifecycle evidence, and an operator must repair the record/report or pause/cancel
the run before recording completion. `EvidenceConflict` is reserved for session
observation conflicts such as inconsistent identity, foreground, or owned-work
evidence; its guidance directs operators to inspect and recheck those observations.

Known background work keeps an active step Running and appears in the Circuit's
current harness observation details as “Waiting for background work”. It has no
timeout or automatic release: if background work finishes without a lifecycle
callback, inspect the harness session and trigger a fresh status report. If the
harness cannot report again, pause or cancel the Circuit and resolve the agent
before starting a new attempt. A Completed attestation is unavailable while the
last lifecycle report still says a question, permission or background task is
outstanding.

The [October 2026 six-harness reliability audit](../archive/2026-10/circuit-harness-reliability-audit.md)
is historical context. Current qualification gaps remain: there is no 30-session
live soak across the six target harnesses, no measured per-harness callback
delivery rate, and no evidence establishing a 99% autonomous Circuit completion
rate. Qualification metrics are tracked in #2127. Harness-owned observation
strategies and diagnostics (#2128) are described under
[Harness-owned observation strategies](#harness-owned-observation-strategies).
Historical signal health and fixture tests cannot establish that target.

Terminal input attribution is streaming: focus notifications and cursor-position
responses do not change the input generation or invent a draft, including when
packets span writes. Real text, edits, paste and submission retain their fences.
Unknown input cannot be silently converted into permission to automate.

A pending Escape or incomplete CSI keyboard sequence must not consume a later,
separate Enter or Ctrl+C event as its final byte. Desktop IPC and mobile
WebSocket input preserve complete xterm `onData` events; raw/programmatic byte
writes have no such framing guarantee. Outside bracketed paste, a standalone
control event re-establishes the boundary and invalidates older input stamps.
Alt+Enter (Escape plus Enter in one event) remains uncertain, including when
delivered as fragments through the raw byte API. Controls in
paste or OSC string payloads remain literal/uncertain; they cannot authorize a
handoff. An unfinished Escape alone remains uncertain, and no timeout converts
it into an empty prompt.

Run 265 (source agent 4636) recorded `input_uncertain` at `await_source` on
September 29, 2026 at 17:38:20 UTC. Its history proves that input attribution
blocked report binding, but does not retain the bytes that caused it. A replay
against the production decoder reproduced Escape followed by Enter or Ctrl+C
remaining uncertain, including across separate writes. The regression covers
decoder recovery and registry report fencing; it is not a reconstruction of
that run's keystrokes or proof that its live checkpoint has advanced. PR 1967's
lifecycle observation changes do not modify this input decoder.

Deterministic preflight precedes inference. A usable candidate carries its
identity and stable report snapshot; an unusable candidate yields a typed blocker
with precise text in step context/history. Do not spend classifier calls on a
report that cannot bind. Reconciliation continues for Unverified steps and can
use validated native turn completion despite stale Running display state.
Codex, Muse and Command Code report snapshots can carry that native completion
fact; other adapters retain their supported observation paths. No generic
terminal silence rule invents completion or background ownership.

### Parked gates are re-judged on new evidence, not on every tick

A classification that cannot be bound — a known blocker, a report that fails the
binding fence — parks the step Unverified. Such a classification still observed a
specific report, evidence owner and blocking state, so the stepper records that
identity (`evaluated_report_revision`, `evaluated_evidence_owner` and
`evaluated_evidence_blocker`) even though no verdict was bound.
`evaluated_evidence_owner` is deliberately distinct from
`classified_evidence_owner`, which means only "a verdict was bound to this owner".

Without those stamps the worker reads an unchanged parked report as never judged
and re-classifies it on every observation tick, appending an identical
`classification` row plus a fresh `step_transition: unverified` and
`checkpoint_reason` each pass, so the ledger grows without bound while nothing
changes.

A refusal is only valid for the state that refused it, so that state is part of
the key. Acceptance reads live state — open owned work, lifecycle blockers,
evidence conflicts — and none of it moves the report revision or the evidence
owner. Keying suppression on those two alone would hide a cleared blocker
indefinitely, leaving the gate Unverified until the agent wrote a new report or
someone pressed Recheck. Re-judging is therefore admitted for a changed report
revision, a new native turn or agent owner, a **changed blocking state** (owned
work completing, a lifecycle blocker clearing, a conflict resolving), or an
operator recheck.

`unverify_step` enforces the ledger half of the same invariant in the state
transition itself: a step already Unverified for the same reason and outcome
writes nothing. Every emitted `StepWrite` becomes a `step_transition` row, and an
`unverified` one also a `checkpoint_reason` row, so an unchanged observation must
not append identical ledger rows. It is not a substitute for the blocker key:
this one only suppresses a repeated *write*, while re-judgement is decided by the
worker gate above.

## Harness-owned observation strategies

Each harness adapter declares one typed `ObservationStrategy`
(`circuit::strategy`, exposed as `AgentProvider::circuit_observation()`). It
normalizes vendor facts into the observation vocabulary above and nothing more:
freshness, requests, known-work blockers and authorization stay with the shared
Circuit policy, and an adapter cannot authorize progression. There is no
`supports_autopilot` flag.

| Part | Meaning | Who executes it |
|---|---|---|
| Push | The adapter's hook parser (`HookParser`) plus the observation sources it records, and an optional explicit ownership gap for a settled turn that reports no registry | Attention route calls the parser for the node's own harness; `native_hooks` normalizes the result |
| Pull | A validated native turn-completion read with its own source names, label and ownership limit | `native_pull` re-reads it while a step is unsettled |
| Identity | `TurnIdentity`: none, native token, or token plus a prompt echo bound to a recorded submission | Diagnostics; the fences themselves remain shared |
| Owned work | Unavailable (with reason) or hook registry | Diagnostics and the ownership fact a settled turn records |
| Final report | Transcript/terminal only, hook message, or pull message | Diagnostics |
| Reconciliation | The bounded yielded-report budget | Watchdog wait observation |
| Passive watcher | The transcript watcher a recovered node must reattach | `restart` |

Today Claude Code and Antigravity declare hooks, Codex declares hooks and a
rollout pull, Command Code and Muse declare a passive watcher, and every other
harness (including MiniMax Code, OpenCode, Grok and Cline) is unwired, so its
missing evidence stays explicit and Circuit execution stays Unverified.

The strategy is chosen from the node's own harness. A node with a launch
snapshot is read as the executor frozen in that snapshot, so remapping a profile
afterwards cannot make the pull read another harness's transcript; otherwise a
harness id, custom harness profile or proxied `harness:account` option resolves
to the harness that executes it. An unknown harness stays unwired rather than
falling back to Claude. Resolving a stored provider reads preferences, so the
evidence history records which provider each row needs and resolves it only
after releasing its database connection. A persisted receipt records the adapter that parsed it, so a replay
after restart reads it with that same declaration. The operator diagnostics
(`observer_policy`) are rendered from the same declaration; its typed
`coverage` is the contract and the prose fields are display text with the
inspected version and platform. Adding a harness means declaring a strategy in
its adapter, not editing the worker.

## Scheduling contract

Classifier and verification work use a bounded FIFO job service: four active
jobs and at most 128 queued jobs. A gate/attempt key prevents duplicate in-flight
work. Queued work is not meaningful agent progress. Queue pressure must defer
observation without converting it to a semantic verdict.
Refused admission attempts increment `admission_deferrals`; structured warnings
report the cumulative count, queue limit and requesting run/step/attempt at
counts 1, 2, 4, 8 and so on. Pending polls do not increment this counter.
The scheduler retries admission; these warnings distinguish saturation from
ordinary pending work without logging every poll.

The circuit worker consumes results and retains transition ownership. Cancellation,
attempt and evidence freshness are checked before a result can commit. Verification
against the same worktree is exclusive. A slow model or verification command must
not hold the scheduler, database connection or global process registry lock.
Existing timeouts and subprocess cleanup still apply. Ambiguous effects retain
their durable claims; moving work to a pool never authorizes replay.

Windows verification passes shell syntax with the shell's quoting rules, rather
than ordinary executable-argument escaping. A reproduced defect returned exit
zero from a quoted PowerShell command without running its intended check.
Regression checks must assert both the side effect and the exit result.

Background watchdog publication validates the run, step attempt and target agent
under the same writer transaction as the lifecycle update. Session/input fences
still apply, with SQLite acquired before the per-agent input guard. A cancelled
or paused circuit cannot publish a stale recovery to its borrowed source.

This isolates these expensive operations. It does not establish that every
external effect, filesystem operation or runtime service is asynchronous.

## Recorded verification

- The complete frontend unit suite with two workers passed: 3,514 tests passed,
  one skipped across 252 files. Lint passed with zero findings; integration tests
  passed all 69 checks.
- The initial full check command failed eight frontend unit tests and two Rust
  tests. The exact eight frontend failures are not established as pre-existing:
  a full baseline run failed a different timeout, while a focused baseline run
  of the three affected files passed 55 tests. The passing two-worker rerun
  suggests resource sensitivity; it does not make the initial run green.
- Real development-profile UI verification displayed the typed blocker and its
  resolution through production IPC. The 240px panel had no horizontal overflow.
  The [inspected screenshot](../pr-screenshots/select-even-hen/session-observation-after.png)
  records the result; the driver is
  [the observation UI scenario](../../tests/integration/ui-shot-circuit-observation.steps.mjs).
  No new panic entries appeared. Startup errors referenced old missing fixture
  repositories and inactive agents. The scenario inserts history rows directly
  with SQL: it verifies backend history reads and the operator surface, not the
  worker-to-database write path. That end-to-end write-path verification remains
  a gap. Separate Rust stepper and history-ledger tests cover their respective
  seams; they do not turn this UI fixture into a worker integration test. No
  autonomous live harness review or fleet soak was performed.
- The final serial Rust library suite passed 3,903 tests, with 24 ignored.
  Separate Rust integration targets passed 18 tests. One initial failure was a
  test fixture node identifier. The other exposed a production Windows quoting
  defect: a quoted verification command could return success without execution.
  After the quoting fix, all 149 circuit-worker tests passed, including real
  quoted success/failure commands and cancellation. The evidence/binding suite
  passed 25 tests after aligning optional wire serialization with its generated
  TypeScript type. Rust doc tests executed none (one ignored).
  All-target Clippy completed with existing warnings; review found no new
  warnings introduced by these changes. The suite does not establish live
  support for every harness/platform combination.

## Acceptance and remaining work

Regression evidence must cross production boundaries: terminal protocol and real
draft attribution; preflight rejecting unusable bindings without inference;
native completion with stale display state; newer input/tools invalidating old
reports; Unverified recovery; cancellation and attempt fences; bounded job
admission, worktree exclusion and unrelated progress during a stalled job.

A production-seam review should traverse source handoff, reviewer dispatch,
findings, fixes, fresh approval and cleanup. Live evidence must record harness
version, platform and launch configuration separately from fixture tests.

Before claiming readiness for 20–30 supervised sessions, complete:

- A unified adapter observation snapshot with versioned capabilities, freshness,
  explicit requests and the precise coverage of child/background ownership.
- A 30-session live soak with delayed publication, missing hooks, restart,
  cancellation and service outages; measure queue delay and operator interventions.
- A fleet exception interface driven by the same typed reasons and evidence,
  with attention priorities and next safe actions.
- Review and merge attestations bound to the exact repository revision, required
  checks and approval policy; lifecycle success never substitutes for these.
- An audit of remaining external effects and runtime blocking, including recovery
  and cleanup, beyond classifier/verification isolation.

The target is supported success paths plus explicit, bounded uncertainty and
safe recovery. Neither this change nor a new classifier warrants a universal
100% reliability promise across external harnesses and services.

## Appendix: Jev and System One classification

Jev is a plausible optional interpretation backend: TypeSafe describes typed
probabilistic decisions over supplied state, rather than generated prose.
Its schema guarantee constrains output shape, not the correctness of a circuit
decision. Vendor speed claims are not Buildmesh measurements.
[TypeSafe introduction](https://typesafe.ai/blog/introducing-system-one-models-and-jev).

Confidence is useful for choosing abstention and escalation policies, but requires
evaluation on our reports. The vendor separately documents confidence and model
limitations. [Confidence](https://docs.typesafe.ai/confidence),
[Jev 1.13 limitations](https://docs.typesafe.ai/model-jaggedness/jev-1.13).

First benchmark an optional backend in shadow mode on authorized, redacted
fixtures: final versus intermediate reports, real questions, permissions,
background progress, review findings and explicit approval. Measure incorrect
advancement, abstention, calibration, latency and availability against the current
classifier. Keep deterministic identity/freshness gates unchanged. This change
adds no Jev dependency, credentials or external report transmission.
