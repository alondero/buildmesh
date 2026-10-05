# Circuit agent loss audit, 2026-10-02

Read-only inspection of the stable profile's SQLite ledger and rotated runtime
logs at base `28ef25d050a11b241aeb76188d67eae24fee3a28` distinguishes the
original checkpoint from the later removal of its agent. The stable ledger,
preferences, agents and application were not changed during this investigation.

## Evidence

| Run | Before removal | Final transition |
| --- | --- | --- |
| 276 | Implementer 4724 first had no session identity/report; after restart, the captured MiniMax session belonged to unrelated work. Later checkpoints rejected newer activity following the report. | Implementer cancelled at 09:56:48 UTC on October 1. |
| 277 | Implementer 4725 handed off its report at 23:45:15 UTC on September 30. Its implementation classifier accumulated 348 failures, despite claiming a five-attempt limit. Later input/report checkpoints also blocked progress. | Implementation classifier cancelled at 15:36:56 UTC on October 1. |
| 280 | Implementer 4730 handed off at 10:11:24 UTC on October 1. Its implementation classifier accumulated 440 failures. The last classifier checkpoint was at 18:05:30 UTC. | Implementation classifier cancelled at 18:06:15 UTC on October 1. |

All three attached agent rows were absent at inspection. Retained runtime logs
show classifier exit 1, but do not preserve its historical stdout/stderr or the
actor that removed each node. The earlier [runs 276/277 investigation](circuit-runs-276-277.md)
reproduced expired Claude authentication in an isolated replay; that is evidence
of the replay failure, not proof of the exact error from every historical call.

Among runs 254 through 280, 21 runs had the same final `piloted agent node was
closed` message. Older runs lacked checkpoint history, so their original causes
cannot all be recovered. The [September 30 audit](../2026-09/circuit-recovery-2026-09-30.md)
also found prompt-delivery uncertainty, input drafts and persistence failures.
Closing an agent after a stuck workflow does not establish why it became stuck.

## Remaining defects and correction

The base already contains MiniMax identity fencing, the durable five-failure
classifier budget, configurable classifier selection and bounded error capture.
The observed counters show that the failure budget was not enforced in those
historical executions. A spawn's
`completed` classification acknowledges report handoff to its downstream gate;
it does not mean task completion was independently classified twice.

Two diagnostic defects remained: agent loss replaced the current checkpoint
with a generic closure message, and failed-run cleanup used that same message
for siblings whose agents had not disappeared. Loss now identifies the agent
and retains its preceding checkpoint. Sibling cancellation names the failed run
as its cause. Deletion, archive and a confirmed Lost status still terminate the
waiting run and release capacity; a database read error is not confirmed loss.

The simpler completion path reuses the existing explicit report contract.
Issue-review implementation, wrap-up and feedback prompts request one terminal
`BUILDMESH_HANDOFF_V1: READY` or `BLOCKED` line. Their matching gates interpret
that line without a classifier process. Missing contracts retain legacy
classification; malformed contracts cannot establish completion. This dispatch
policy applies without rewriting saved prompts or active graph snapshots.

The ordinary session, input, report freshness and known-work preflight still
runs. READY is a report-based handoff for the assigned phase, not review approval
or proof of complete native work ownership. PR publication checks and the
independent review verdict remain separate gates. Explicit evidence recheck is
still required after an exhausted classifier budget; a new result cannot silently
replenish it. Already deleted agents and failed historical runs are not recreated
or marked successful by this patch.

## Verification

Before correction, regression tests reproduced lost checkpoint explanations,
mislabelled sibling cancellation and missing issue-phase dispatch contracts.
The final Windows serial Rust check passed 4,020 library tests (27 ignored),
plus integration targets with 8, 1 and 9 passing tests. The report-admission
regression reads a real MiniMax-format fixture, admits its explicit phase result,
then rejects draft input, missing identity, pre-incarnation/pre-prompt reports
and known unfinished child work. The recorded classification remains separate
from native lifecycle verification.

Frontend build, ESLint and its violation fixtures, 33 documentation tests and
11 agent-infrastructure tests passed. All-target Clippy passed with 4 library
and 36 library-test warnings (2 duplicate diagnostics), reproduced at the
recorded base with matching warning messages and source files; no new warning
was introduced. Generated TypeScript bindings did not change. Independent
Standards and Spec reviews found no remaining blocking findings.

These tests establish the production parsing, admission, transition and
persistence behavior. No fresh autonomous issue run or fixed stable-app rollout
was performed; a rebuilt application and a new run are still required for that
runtime evidence.
