# Circuit session observation and autonomous supervision

Audience: circuit and harness maintainers. Status: architecture assessment and
implementation contract, September 26, 2026. Recorded verification is below;
remaining acceptance requirements are not claims of executed tests.

Buildmesh should let an operator supervise many sessions by surfacing actionable
exceptions while ordinary work progresses independently. The existing pure
stepper, transactional ledger, identity fences and effect journal support that
direction. Reliability requires correcting the observation and scheduling
boundaries, rather than replacing the workflow with another model judgement.

## Diagnosis

Historical failures repeatedly made observation depend on the state it needed
to establish. [Run 86](circuit-run-86-observer-recovery.md) needed a watcher to
publish yield, but watcher recovery waited for yield. [Run 122](review-circuit-handoff-audit.md)
had native completion while its display remained Running. [September 26 stalls](circuit-failures-2026-09-26.md)
required ownership coverage unavailable from installed harnesses and stopped
observing Unverified steps. [Run 104](circuit-run-104-watchdog.md) shows the
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

Terminal input attribution is streaming: focus notifications and cursor-position
responses do not change the input generation or invent a draft, including when
packets span writes. Real text, edits, paste and submission retain their fences.
Unknown input cannot be silently converted into permission to automate.

Deterministic preflight precedes inference. A usable candidate carries its
identity and stable report snapshot; an unusable candidate yields a typed blocker
with precise text in step context/history. Do not spend classifier calls on a
report that cannot bind. Reconciliation continues for Unverified steps and can
use validated native turn completion despite stale Running display state.
Codex, Muse and Command Code report snapshots can carry that native completion
fact; other adapters retain their supported observation paths. No generic
terminal silence rule invents completion or background ownership.

## Scheduling contract

Classifier and verification work use a bounded FIFO job service: four active
jobs and at most 128 queued jobs. A gate/attempt key prevents duplicate in-flight
work. Queued work is not meaningful agent progress. Queue pressure must defer
observation without converting it to a semantic verdict.

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

## Jev and System One classification

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
  repositories and inactive agents. This fixture verifies the operator surface,
  not an autonomous live harness review or fleet soak.
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
