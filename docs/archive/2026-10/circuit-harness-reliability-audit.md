# Autopilot Circuit harness reliability audit

Date: 2026-10-07. Baseline: `57e2f44a09e3b995dfb2cbdccc4e7df004e2e0b2`.

## Finding

Buildmesh cannot currently substantiate 99% autonomous Circuit completion across
Codex, Claude Code, Antigravity, MiniMax Code, OpenCode and Grok Code. There is no
fleet success denominator, version/platform qualification or end-to-end soak
establishing that rate. Safe uncertainty is intentional, but an Unverified step
requiring intervention is not autonomous success. An accepted callback and a
green fixture suite do not establish reliable delivery from the real runner.

## Required observations

| Fact | Required information | What it must not imply |
|---|---|---|
| Session/process | Harness and version, platform, node, session incarnation, native session ID, alive/exited/lost, exit reason | A live process is doing model work |
| Submission | Assigned prompt digest, delivery acknowledgement, input generation, native turn ID when available | Writing bytes means the harness accepted them |
| Foreground | Starting, working, yielded, completed turn, cancelled or failed; event source and time | A completed turn means the task succeeded |
| Owned work | Stable child/task IDs, starts and terminal outcomes, explicit coverage of the inventory | Missing inventory means no background work |
| Human request | Question, permission or review approval; request ID, owner, reply and disposition | Generic attention means permission, or a report answers a question |
| Report | Session/input-bound revision, publication time, completeness, currentness | A plausible final sentence is fresh evidence |
| Observation health | Supported capabilities, observed delivery, rejected/stale events, reconciliation outcome | Installation or historical health is a heartbeat |
| Circuit authorization | Run/step/attempt, cancellation, current evidence and semantic verdict, external checks/approval | Lifecycle success authorizes merge or proves correctness |

These are independent facts, not one enum. The useful operator states are Starting,
Working, Waiting for background work, Ready, Needs an answer, Needs permission,
Unverified, Failed/Lost, and automation-completed. Unknown evidence remains unknown.

## Six-harness inventory

This table describes the checked-in implementation, not qualification of the latest
vendor release. Native Circuit authority is stronger than node-status support.

| Harness | Node observation | Circuit evidence path | Remaining limit |
|---|---|---|---|
| Codex | Native hooks for turns, questions, permissions and background signals | Identity/input/time-fenced rollout `task_complete` pull plus native request receipts | No complete owned-work registry; cross-platform qualification and restart-time question recovery remain open |
| Claude Code | Native prompt/stop/request/child hooks and transcript background evidence | Prompt echo plus `prompt_id` binds a recorded submission; Stop report, child events and explicit task/cron registries | Missing tokens/registries reduce confidence; current policy explicitly calls live delivery unverified |
| Antigravity | Stop distinguishes `fullyIdle: false` from foreground settlement | Session/incarnation-fenced Stop receipt and fresh transcript report | No authoritative turn/input token or complete ownership registry; a settled foreground cannot prove owned-work completion |
| MiniMax Code | Plugin completion only is advertised | Fresh report reader, input/session fences and report handoff contract | No native Circuit lifecycle adapter; completion does not cover human requests or general owned work; idle background-result wake-up gap #2105 |
| OpenCode | Plugin busy/idle/question/permission/reply events | SQLite report snapshots and guarded report handoff | No native Circuit lifecycle receipt, turn/input token or complete ownership registry |
| Grok Code | Native turn/input/question/permission events | Fresh transcript report and guarded handoff | No native Circuit lifecycle adapter or complete ownership registry; qualification #1903 remains open |

Source owners: `agent/provider/adapters`, `http/routes/attention/normalizers`,
`services/circuit_worker/{observer_policy,native_hooks,codex_observer,readiness}`,
`services/transcript_reader/report_snapshot`, and `circuit/observation`.

Local Windows `--version` probes in this session returned Codex 0.160.1,
Claude Code 2.1.293, Antigravity 1.3.1, MiniMax Code 0.6.3, OpenCode 1.18.3 and
Grok 1.0.46. These are executable/version checks only. In particular, the
Antigravity observer policy cites validation against 1.2.11; installation of
1.3.1 is not fresh evidence that its callback contract still matches.

## Critical defect reproduced in this session

The shared report preflight checked the Circuit evidence ledger but ignored the
node's valid persisted lifecycle snapshot. Generic AwaitingInput projections
deliberately do not establish a request; meanwhile OpenCode and Grok do not enter
the native Circuit receipt parser. A current question/permission could therefore
coexist with an admissible final report. BackgroundRunning had the same omission,
including Antigravity's non-authoritative busy receipt.

The transactional classification commit checked session/input identity but did not
recheck these lifecycle blockers. A request arriving during classification could
therefore be missed even after adding a preflight check. Watchdog recovery also
needed to preserve observed background work instead of replacing it with Ready.

The fix uses one harness-independent negative-evidence policy at report admission,
classification commit, and recovery. A normalized question/permission blocks report
handoff; BackgroundRunning blocks it as known outstanding work. Generic InputRequired
remains distinct. A later valid lifecycle transition can release the snapshot
blocker; stronger durable request/child evidence remains separately enforced.
Positive lifecycle snapshots never confer completion authority.

## Seam assessment

The pure stepper, transactional ledger and report-readiness interface are useful
existing seams. HTTP normalizers are already separate per harness with replay and
cross-harness isolation tests; #1879 is closed. New integrations must supply their
own parser and cannot borrow another harness's payload vocabulary.

The split is incomplete. The worker still owns a provider switch in observer_policy,
a Claude/Codex/Antigravity parser in native_hooks, and a Codex-specific observation
entrypoint. Capability prose, node capabilities and Circuit strategies are separate
inventories. Extending a harness can require changes in several owners. Follow-up
work should give the harness adapter a typed observation strategy (push, pull,
coverage and reconciliation), with one shared Circuit policy consuming facts.
Do not hide these differences behind a boolean `supports_autopilot` or move Circuit
authorization into a vendor adapter.

## Reliability acceptance

Define separate measurements before setting a release claim:

- **Safety:** no advancement during an unresolved request, known owned work,
  stale identity/report, cancelled attempt or missing required approval.
- **Availability:** eligible runs reach their intended terminal outcome without
  manual repair within an agreed deadline. Unexpected Unverified stalls count as
  failures. Intended human waits are reported separately, with response/recovery
  latency measured after an answer.
- **Observation:** callback delivery, false Ready/Working/request classification,
  reconciliation delay, duplicate rejection and restart recovery, split by harness,
  version, platform and fresh/resumed launch.

For an illustrative independent, identically distributed zero-failure sample,
299 successes give a one-sided 95% lower confidence bound above 99%
(`0.05^(1/299)`). This is not a guarantee and does not address correlated outages,
upstream version drift or sparse scenarios. Pooling six harnesses can hide a broken
one; qualification needs each supported configuration plus deliberate fault cases.
With ten independent indispensable stages, 99% per stage gives only
`0.99^10`, about 90.4%, end-to-end success.

The qualification corpus should cover ordinary completion, delayed/missing/duplicate
hooks, concurrent nodes in one directory, tool denial, multiple questions, child
completion before start, background work, malformed/partial reports, long prompt
delivery, restart, cancellation during classification, listener outage and upgrade.
Record actual outcomes and operator interventions, not only test counts.

## Existing unresolved work

- [#2127](https://github.com/alondero/buildmesh/issues/2127): measurable per-harness 99% reliability qualification.
- [#2128](https://github.com/alondero/buildmesh/issues/2128): harness-owned Circuit observation strategies and capability diagnostics.
- [#1964](https://github.com/alondero/buildmesh/issues/1964): real-runner delivery and transport-to-client validation.
- [#1965](https://github.com/alondero/buildmesh/issues/1965): durable observation revisions and process-incarnation fencing.
- [#1895](https://github.com/alondero/buildmesh/issues/1895): controlled live human-answer-only gate proof.
- [#1896](https://github.com/alondero/buildmesh/issues/1896): interrupted Codex question recovery after restart.
- [#1903](https://github.com/alondero/buildmesh/issues/1903) and [#1904](https://github.com/alondero/buildmesh/issues/1904): Grok and cross-platform Codex qualification.
- [#2105](https://github.com/alondero/buildmesh/issues/2105): MiniMax background-result wake-up.
- [#2108](https://github.com/alondero/buildmesh/issues/2108) and [#2061](https://github.com/alondero/buildmesh/issues/2061): long-prompt delivery and paste confirmation.

These remain release-reliability blockers even where the safe behavior is to stop.
They are not evidence that this session measured a fleet failure percentage.

## Verification recorded on 2026-10-08

All three regression scenarios failed at their intended assertions before the
fixes. Afterward, `cargo test --lib report_` passed 63 tests and
`cargo test --lib circuit_recovery` passed four. These exercise production report
readiness, SQLite commits and recovery, not real model sessions.

The first `npm run verify` failed two Windows Codex subprocess tests with
`program not found` / `powershell.exe` not recognized. All other Rust test
processes passed. The prescribed rerun with `BUILDMESH_RUST_TEST_JOBS=1` passed
all 4,428 Rust tests and all 17 verification gates, including Clippy, formatting,
documentation, lint and generated-binding drift. The initial failure is retained
as evidence in [#2109](https://github.com/alondero/buildmesh/issues/2109#issuecomment-6048622210);
an equivalent baseline reproduction was not performed, so its cause remains
unverified. Neither a focused pass nor the serial rerun erases that failure.

Independent Standards and Spec reviews approved the change. No live six-harness
model-session exercise, packaged desktop test or fleet soak was performed.
