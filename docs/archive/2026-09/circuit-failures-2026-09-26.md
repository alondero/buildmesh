# Circuit stalls on 2026-09-26

Investigated from `41cfdc29c7eefe621d388df9c3c24f2a24b96fd1` using read-only
queries of the stable profile ledger, its rotated application log, and native
session transcripts. The stable process and its database were not modified.

## Evidence and causes

| Runs | Observed state | Cause |
| --- | --- | --- |
| 229–231 | Source reports classified completed, steps Unverified, no native ownership coverage; source agent rows subsequently deleted | The strict completion change required ownership proof unavailable from these harnesses. The worker then stopped observing Unverified steps, so even confirmed source removal could not release their slots. |
| 232 | Codex source 4495 has a final report and native `task_complete`; the ledger has a status-projection input wait | `AwaitingInput` was converted into an indefinite human request without a request identity or native request. No corresponding response could ever resolve this invented wait. |
| 233–237 | Pending on mesh 65 | Runs 229–232 occupied all four admitted-run slots. The log repeatedly reports four active runs against capacity four. |
| 238 | MiniMax Code source 4509 is AwaitingInput; its native transcript ends with a shell tool call, without a later final answer | This is unfinished work, not a completion-report case. Removing the inferred indefinite wait restores a bounded evidence checkpoint; the outstanding tool does not become success. |

The preceding failures were also inspected: 224 and 226 expired without session
identity/report, 225 expired after two hours without progress, and 220/228 lost
their reviewer. Those historical records alone do not establish why their
harnesses stopped or failed to publish identity. This change does not claim to
repair those harness processes.

## Completion policy

The old policy was safe against premature completion but had no reachable success
path for the affected installed harnesses. Circuit progress now distinguishes a
guarded report decision from verified native lifecycle/ownership facts:

- A readable report belongs to the current session incarnation and input. Its
  revision, text, publication time and short-lived transcript snapshot travel
  with the classifier result to the commit boundary.
- Newer user/tool activity and partial publication reject the report. Codex
  requires its current native completion record; Command Code and Muse reuse
  their existing turn trackers. Publication time comes from the native record,
  not a later file modification.
- Filesystem/report validation runs before acquiring the Buildmesh DB writer.
  The existing process input guard and transactional session check still protect
  the durable transition. Report-based history does not claim verified native
  lifecycle or invent a complete owned-work registry.
- Known unfinished children, evidence conflicts, actual questions and actual
  permissions remain blockers. A generic status projection is retained as an
  observation, but cannot create an indefinite human request. Old persisted
  projection-only waits are interpreted consistently after upgrade.
- Assigned spawn steps can hand a finished report to their downstream classifier;
  reviewer readiness and explicit review verdicts remain separate decisions.
- Unverified agent steps keep observing for reports and confirmed loss/error.
  This never replays an ambiguous spawn, prompt, or GitHub mutation. New reports
  are checked on activity; unchanged checkpoint reports use the classifier
  cooldown rather than issuing a classifier request on every worker tick.

This is a deliberate practical completion policy, not a claim that a report
proves the absence of every background process. Harnesses that cannot expose
background ownership retain that limitation in their displayed capabilities.

## Regression evidence

The status-projection wait and lost-agent checkpoint tests were both executed
against the faulty implementation and failed on the observed conditions. Tests
also cover confirmed errors beneath lineage gates, unchanged/fresh report
snapshots, tool and user suffixes, partial records, newer native turn starts,
known unfinished work, wrong attempts, stale reports rejected before commit,
and the built-in review graph reaching explicit approval from guarded reports.

Windows verification passed the full `scripts/check.ps1 rust -SerialRust`
gate (3,853 unit tests and 18 integration tests; 25 ignored including the doc
test). After the final review corrections, focused checks passed 538 circuit
tests, six report-snapshot tests, 13 Muse watcher tests (one ignored), and 15
Command Code watcher tests. The mobile build, 19 documentation tests, document
contract and agent diff rules also passed.
`cargo clippy --locked --all-targets` completed successfully with warnings in
unchanged code; no warnings point at added or changed lines in this patch.

These tests replay the production reader, stepper and commit validation
boundaries; they do not launch a live reviewer or claim an end-to-end stable-app
success. A rebuilt application is required for the live queue to recover.
