# Circuit recovery audit, 2026-09-30

Read-only inspection of the stable profile ledger and rotated application logs
from base `9f8b94c447321a20ca00c02c55ca2d650adb6786` found 18 failed, two completed,
two running and three pending runs among runs 254 through 278. Many failed runs
were terminalized after their attached agents were closed; that final state
alone does not establish the original reason for their checkpoints. The stable
database and running agents were not changed during this investigation.

## Reproduced causes

| Evidence | Cause | Change |
| --- | --- | --- |
| Run 278 repeatedly classified its reviewer report as completed, then logged `Observation freshness fence rejected: agent session incarnation changed before evidence commit`; the visible checkpoint eventually said its 120-minute evidence window ended. | Readiness accepted a finished report despite a stale Running projection, but the SQL commit predicate required Ready/AwaitingInput/Completed whenever a report snapshot was present. It misreported this status mismatch as an incarnation change. | Keep session, input and report freshness fences, but do not repeat the incompatible display-status gate at commit. |
| Run 274 found its PR target on the second OpenPr attempt, then logged `commit failed: Query is not read-only` and became uncertain. | Inheriting a newer upstream attempt cleared error/outcome in memory without emitting a fresh-attempt reset. The ledger retained the previous wrap-up failure, so the next transition's expected steps differed. The generic SQLite InvalidQuery text obscured this consistency failure. | One step setter owns inherited attempt resets and emits the matching durable reset; remove the earlier caller-side reset. |
| Runs 265, 267, 268 and the second feedback attempt in 269 retained prompt intent without acknowledgement; several runs also retained InputDraft blockers. | Unknown prompt delivery was deliberately not replayed. Operator recovery additionally required all ancestor reviews to be approved, which cannot hold on a changes-requested feedback route and does not model alternative approval branches. | Recover already-admitted feedback by attesting observed delivery; current human requests and known work/conflicts still block. Agent handoffs gain a separate operator completion path. |

The report commit and inherited-attempt regressions were executed before their
fixes and produced the exact two live error strings above. Prompt-delivery
history does not retain the original PTY screen or prove why a rendered-paste
wait failed. This patch does not claim to repair every harness's paste protocol
or turn a staged draft into a submitted prompt.

## Recovery contract

Uncertain OpenPr attempts with a saved repository and branch receive up to five
read-only lookups, a minute apart. The allowance is persisted before lookup;
restart cannot replenish it. A matching result restores PR identity and advances
the existing attempt. Absence or lookup failure does not authorize another
creation. A manual recheck remains possible after exhaustion.

An operator can record completion of an eligible attached-agent handoff with a
reason. The view and command share the eligibility policy. The transaction
fences run state, step attempt and history revision, preserves native evidence,
and records a distinct operator attestation. The ordinary worker tick schedules
the next step. This cannot establish a missing spawn identity, supply a review
verdict, answer a human request or override known unfinished work/conflicts.

For a staged feedback prompt, the operator submits it in the terminal and
confirms acceptance before attesting delivery. No recovery action silently
clears a draft, presses Enter or repeats a possibly applied external action.

Step transitions persist the same outcome used by the in-memory graph, including
inherited retries, so reload does not lose outcome-based routing. Checkpoint
explanations are scrubbed and retained in history when recheck or cancellation
replaces the current error. Successful operator recovery clears that current
error while retaining the operator's reason in the audit trail.

## Verification boundaries

Regression coverage crosses report readiness into the database commit, a
retried step through the worker's result-persistence path, and OpenPr dispatch
through a scripted HTTP endpoint followed by restart and automatic read-only
reconciliation. Operator tests retain audit provenance, reject stale requests
and protected gates, preserve changes-requested verdicts, and allow the next
ordinary stepper tick to continue.

See the PR evidence for executed suite counts and dev-window checks. Harness
execution and live GitHub publication are distinct from these deterministic
tests; existing stable runs need a rebuilt application to receive the fixes.
