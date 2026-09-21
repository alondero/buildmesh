# Circuit run 163: a mid-work reviewer yield consumed as the review verdict

Investigated on 2026-09-20 from `3e13c289`. Evidence came from read-only
inspection of the stable profile's SQLite ledger (`mode=ro`), `buildmesh.log`,
the reviewer's own MiniMax Code session store, and the Claude-compatible
transcript mcode publishes for its hooks. The stable process, its ledger, and
the reviewed agent's worktree were not changed.

## What run 163 did

Circuit 6 ("Review agent 3534") reviewed agent 4252 (`unhurt-vulgar-wick`).
The reviewer provider for this run was `mcode` (`review.provider`), so the
`verdict` gate's target was mcode node 4260.

All times below are UTC.

- 19:38:46: the run starts; `await_source` borrows node 4252.
- 20:02:42: the source reports and `source_ready` completes; the reviewer is scheduled.
- 20:11:40: review round 1's reviewer session starts.
- 20:13:11 → 20:14:28: round 1 yields `BLOCKED`, feedback is delivered, `await_fixes`
  completes at 20:17:32 with the source's fix report, and `retry` re-runs the reviewer.
- 20:24:00.979: node 4260 (mcode) is spawned as round 2's reviewer.
- 20:26:51.458: the reviewer hands the full unit suite to a managed background
  task (`bg_53e33c20-…`) and yields; the task is recorded in mcode's runtime
  ledger as `failed` at 20:28:03.219.
- 20:28:03.368: mcode re-invokes the conversation with `<background-task-finished>`.
  The model answers with a text line plus a `bash` retry, and mcode's
  Claude-compatible transcript is last written at 20:28:06.808 with that
  assistant row as its final record (no tool result follows).
- 20:28:09.239930: `session_lifecycle` records `Node 4260 awaiting user input (Node Turn)`.
- 20:28:15.300152: `autopilot evaluator(4260): classified turn as Some(Blocked)`.
- 20:28:15.301056: `turn classifier for run 163 step verdict agent 4260 → Some(Blocked)`.
- 20:28:15: `close_blocked` kills the reviewer; the run ends `failed`.

The ledger preserved the reviewer's *intermediate progress line* as its report:

```
node.reviewer.output        = "PowerShell NativeCommandError. Let me retry with
                               the standard `.cmd` shim correctly and pick off the
                               impacted suites plus a broader window."
node.verdict.evaluated_output = <same line>
node.verdict.review_verdict   = "blocked"
```

Six seconds separate mcode's last transcript row and the kill, and the row is an
assistant `tool_use` with no matching result: the reviewer was still working.
The `BLOCKED` verdict is therefore a verdict about a progress message, not about
a review.

## The PowerShell NativeCommandError is not a Buildmesh fault

`NativeCommandError` does not appear anywhere in `buildmesh.log`. It exists only
in the reviewer's captured output, and the reviewer's own transcript shows why:
mcode's shell tool ran the retry as
`$ErrorActionPreference = 'Stop'; … & 'F:\src\buildmesh\node_modules\.bin\vitest.cmd' run …`.
Under PowerShell 5.1 that preference promotes the shim's stderr into a
*terminating* `NativeCommandError`, so the background task failed and the model's
next line was the retry notice quoted above. This is the exact trap
`scripts/check.ps1` documents and works around locally; it is a property of the
reviewer's shell usage, and Buildmesh neither produced it nor could see it.

The reason that line reached the run's verdict at all is the gate defect below.

## Cause

`classify_step_turn` classifies a gate's report as soon as the target agent is
`AwaitingInput`, `Ready`, or `Completed`. For the `verdict` gate,
`classify_gate_report` then asked one question of the report — the verdict
question (`review_prompt`). Its vocabulary maps a report to a *review outcome*
(approve / changes requested / blocked), so an intermediate progress line can
only answer `BLOCKED`. The gate had no equivalent of the turn-readiness question
(`review_turn_prompt`) that runs 85 and 104 added for the source-bound gates,
even though those two runs established the policy: a yield makes an agent
eligible for inspection, not finished. Run 163 is the same class of failure,
one gate downstream — the reviewer's own yield instead of the source's.

Two properties of the mcode reviewer widened the window. mcode hands its hooks a
Claude-compatible transcript that it writes per turn under
`%TEMP%\minimax-plugin-hooks\transcripts\<sha256(session)[:32]>.compatible.jsonl`
and deletes when the turn closes, so the attention route's Claude Code
background-task scan is at best inconclusive for it (mcode's own background
notice uses different wording, so a launch is never detected); and the route
logged nothing for a `MarkInput` disposition, which left the ledger and log
unable to explain the 20:28:09 Node Turn after the fact.

Which `MarkInput` branch produced that Node Turn cannot be established from the
record, because the route logged nothing for it. Two remain: the transcript scan
returning `None` (an unreadable ephemeral file degrades a plain `Stop` to
"the user is needed"), and mcode emitting a `PermissionRequest` — which
Buildmesh provisions a handler for while `attention_capability` advertises that
a permission signal is impossible by construction. Both end in the same lost
verdict, and the gate fix below prevents the failure either way.

## Changes

- The `verdict` gate now confirms the reviewer's turn is finished before it
  consumes the report as a verdict. An `AwaitingInput` yield is judged by a
  **reviewer-specific** readiness question first
  (`evaluator::reviewer_turn_prompt`); `WORKING`, `CONTINUE`, and an unavailable
  classifier leave the gate waiting with the reviewer running, so its next
  report is judged on its own merits. A finished report, and a reviewer that
  will not continue without a person, still reach the verdict prompt unchanged.
  `Ready` and `Completed` yields keep the existing fast path.
  The dedicated question matters: the implementation gate's `review_turn_prompt`
  counts "reports a provider/API failure" as `BLOCKED`, so run 163's
  `PowerShell NativeCommandError. Let me retry …` line could answer `BLOCKED`
  and reach the verdict classifier after all. The reviewer question treats a
  failure the reviewer is retrying as `WORKING`, and reserves `BLOCKED` for a
  reviewer that needs a decision, a permission, credentials, or a person.
- The wait is published as `TurnParked`, not as a verdict and not as an outage.
  The stepper's only vocabulary for "no classification" is `unavailable`: it
  writes `node.<gate>.classification = "unavailable"`, a user-visible step error
  naming the Mesh Autopilot provider, spends the five-strike classifier-failure
  budget that ends a wedged run, and records the in-progress line as the gate's
  output. `TurnParked` instead records the observation alone — the evaluated
  attempt and report — so the step keeps `Running` with no classification, no
  *classifier-outage* error, no failure budget, and no output stamp. The gate's
  generic yielded-wait reason (*"Waiting for a fresh agent report; retrying
  observation every 10 seconds."*) and its deadline still arrive from the
  separate `WaitObserved` event, which is what bounds the wait; the park does
  not suppress that. Recording the observation is also what bounds classifier
  cost while a reviewer sits mid-turn: `should_classify_report` refuses an
  unchanged report, so the readiness question is asked once per distinct report
  (and, after a recorded outage, on the existing 60-second retry), not once per
  2-second tick.
- An unavailable readiness classifier waits rather than guessing. This gate
  cannot separate a progress line from a final report without it, and reading a
  progress line as a verdict is the failure this change exists to prevent, so
  the wait is the safe answer. The trade-off is explicit: a *sustained*
  classifier outage on a reviewer's verdict gate shows up as the gate's wait
  deadline — fifteen minutes by default, or the step's own budget — plus a
  warning, instead of the provider-naming message the `unavailable` path
  produces on other gates.
- The attention route logs every disposition. `Running` and `MarkInput` were the
  two decisions that produced no log line, so a node could change lifecycle
  state with nothing in the record saying why.
- The user-visible behaviour change is recorded in
  [the v1.4.0 release note](../releases/v1.4.0.md).

## Limits

- This does not make mcode's turn signal authoritative. When mcode reports a
  clean `Stop` while background work is still in flight, the route can still
  publish a finished turn, because Buildmesh's only pending-work evidence is a
  Claude Code transcript scan that mcode's projection cannot satisfy. Closing
  that needs mcode-owned evidence: pending background tasks counted from mcode's
  own canonical history, or a pending-work signal on its hook envelope.
- A waiting gate is re-judged when the reviewer's report changes, when the
  probe's own retry interval admits a fresh observation, and otherwise runs
  until the reviewer's wait deadline expires. A reviewer that yields mid-work
  repeatedly therefore stays live for as long as it keeps working rather than
  failing, and a reviewer that yields once and never reports again is bounded by
  that deadline rather than by this change. Neither is a silent state: each wait
  logs, and the step stays `Running` on the run card.
- Verification is source-level. The gate decision is covered by
  `verdict_gate_waits_for_a_reviewer_that_yielded_mid_turn`, which also pins the
  question the gate asks — it asserts the reviewer readiness prompt, and that
  the implementation gate's "reports a provider/API failure" clause is absent
  from it. The recording contract is covered by
  `turn_parked_records_the_observation_and_nothing_else` (the step keeps
  `Running` with no error written by that event, no classification, no failure
  budget, and the observation recorded) and the wait's bound by
  `parked_observation_is_not_reclassified_until_the_report_changes`. Neither can
  fail against the pre-change code, because the functions and the event they
  drive did not exist; the red/green evidence for the old behaviour is the
  pre-fix probe recorded in this investigation, where the verdict prompt
  received the run-163 progress line. No live circuit run was started, so no
  claim is made about how a model answers the new readiness question in
  practice — the prompt is pinned, its accuracy is not, and existing running
  application processes require a rebuilt binary before this change affects
  them.
