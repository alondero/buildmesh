# Circuit effect recovery contract

Status: current

This contributor reference maps every effect emitted by the Circuit stepper to
its durable intent, dispatch, result, cancellation, and restart policy. It
describes the current implementation and the automated evidence that pins it.
The [September 30 recovery audit](../archive/2026-09/circuit-recovery-2026-09-30.md) records the
live failures behind the report-commit and inherited-attempt regression tests.
The user workflow for uncertain actions is in
[Agent Node Circuits](agent-node-circuits.md).

## Effect inventory

| Effect | Durable intent and dispatch | Result or uncertainty | Cancellation and restart policy |
|---|---|---|---|
| `SpawnAgentNode` | `circuit_effects(kind=spawn)` records `intent` in the transition transaction. A claim changes it to `possible_dispatch` before allocation or prompt delivery. | Attachment and `acknowledged` history commit together under the run, attempt, and prior-agent fences. A missing attachment after dispatch becomes Unverified. | An unclaimed intent may be claimed. `possible_dispatch` never dispatches again after restart or timeout; inspect retained agents and record `not_performed` before a new attempt. Cancellation fences late attachment. |
| `InjectPty` | `circuit_effects(kind=prompt)` records `intent`; the worker claims `possible_dispatch` before writing PTY input. | Successful dispatch persists an acknowledgement before emitting `PromptDelivered`; this records delivery, not task completion. Correlation history returns its updated revision to the worker. | A durable acknowledgement can complete the step after a concurrent update or restart without another PTY write. A claim without acknowledgement remains uncertain and is never automatically resent. |
| `ContinueAgentTurn` | The classifier transition records delivery `claimed`, attempt, and continuation count in the same commit that schedules the prompt. History records intent and possible dispatch. The worker checks process, input, and report revisions before writing. The missing-result-file reminder (issue #2134, see [Circuit architecture](circuits.md#circuit-architecture-spec-1205)) is a second producer of this effect: its transition records the same `claimed` delivery, fenced to one reminder per report revision and a budget of two per step attempt, counted apart from classifier continuations. A borrowed source agent is never reminded. | Delivery becomes `delivered`, `obsolete`, or `uncertain`. | A `claimed` delivery becomes Unverified after restart and is never replayed. The worker can promote an explicit `pending` retry marker to `claimed`; the current classifier path writes `claimed` directly. Fresh agent progress may resolve uncertainty. Cancellation fences a late delivery result. |
| `CallGithub(AddLabel)` | `circuit_effects(kind=github)` records intent and possible dispatch before the GitHub POST. | A successful response acknowledges the action; timeout, cancellation, or restart after claim is uncertain. | No automatic replay. Read-only Recheck is unavailable for this action; inspect GitHub and record an outcome. Retry requires `not_performed` and a new attempt. |
| `CallGithub(RemoveLabel)` | Same `github` claim; GitHub receives a DELETE. | The adapter treats an already-absent label as success. A lost response still leaves the Circuit result uncertain. | No automatic replay or read-only Circuit Recheck. Operator outcome is required before retry. |
| `CallGithub(PostComment)` | Same `github` claim; GitHub receives a comment POST. | A successful response acknowledges the action. An unknown result may have created a comment. | No automatic replay or read-only Circuit Recheck. Operator outcome is required before retry. |
| `CallGithub(CloseIssue)` | Same `github` claim; GitHub receives a PATCH setting the issue state to closed. | Closing an already-closed issue is idempotent, but an unknown response remains uncertain. | No automatic replay or read-only Circuit Recheck. Operator outcome is required before retry. |
| `CallGithub(OpenPr)` | Same `github` claim. Before lookup or create, the worker stores the exact owner, repository, and head branch. | A matching read-only lookup acknowledges the original attempt. Missing, mismatched, or unavailable lookup stays uncertain. | Recovery performs only the saved-target lookup; it never creates a PR. A matching result commits PR identity and effect reconciliation together. Absence stays uncertain. Operator retry still requires `not_performed`. |
| `CallGithub(ConfirmPrMerged)` | Same `github` claim. Read-only: it reads the run's pull request (`GET /pulls/{n}`) and mutates nothing. | Merged is success. An open or closed-unmerged PR, an unreadable PR and a missing PR number are all a routed `Failed` outcome that records `merge.unconfirmed_reason`; none of them fails the run or closes anything. | A crash between the claim and the result leaves an Unverified checkpoint like any GitHub action. The lookup is safe to repeat, but the claim is not replayed automatically, and recording it completed is an operator attestation of the merge. |
| `SetNodeStatus` | The target status write, completed step, and `effect_result` history commit in one SQLite transaction. It has no remote dispatch window. | The transaction either commits all three or rolls them all back. If the target is missing or archived, the worker records the effect step and run as failed in a second transition. | Restart observes the status and completed step together; no replay is needed. The failure transition uses the original run/step fence, so cancellation or deletion that wins before it prevents the failure write too. |
| `CloseAgentNode` | The step completes before resource retirement. Agent deletion records its lifecycle cleanup lease; the target spawn association remains until retirement succeeds. | Deletion is idempotent. A missing agent row is treated as an already-finished close, then the association is cleared. | While the completed close still resolves to its original agent, worker observation emits `CloseAgentRetry`. Once the association clears, retry stops. Cancellation and circuit deletion use the durable retirement path. |
| `Notify` | Emits the `circuit-notification` Tauri event after the run transition commits. There is no notification outbox or delivery acknowledgement. | Notification delivery is transient and best effort. The run transition and history remain durable. | Notifications are not replayed after restart, avoiding duplicate transient toasts. The UI refreshes from durable run state. |

The external journal has a closed Rust kind vocabulary: `spawn`, `prompt`, and
`github`. GitHub subactions share the journal fence but keep distinct provider
contracts above. Worker mapping explicitly handles every `Effect` variant;
adding one requires choosing a durable policy in that match. Unknown external
results never authorize automatic replay.

## Review continuation

Continue review creates a new run from the retained review configuration. The
successor graph contains the reviewer and feedback loop; it omits the
implementation step and every GitHub publication step. Creation, snapshot,
lineage history, and successor identity are committed atomically. The
`recovery.from_run_id` lineage is the deduplication key, so repeating the
request after restart returns the existing successor. A cancelled successor
closes that lineage; a failed successor may be continued as its next
generation.

## Operator and test fences

Multiline Codex and Muse prompt delivery waits for a complete matching paste
echo (a marker, full visible text for short drafts, or the draft's tail for
mid-size pastes drawn inline) and one second of quiet
output before sending Enter. Marker matching ignores terminal padding and line
breaks while retaining the exact character
count and closing bracket. Which confirmation applies is declared per harness
by its adapter (`paste_gate_policy`), not by harness-name checks in delivery.

Muse Code 1.3.0 draws a mid-size paste in full in its input box and only
collapses larger ones to the marker. Probing a real Windows ConPTY on 2026-10-05
showed 600 and 839 raw characters drawn in full and 1,509 collapsed to
`[Pasted Content 1509 chars]`; the exact collapse point (characters or lines)
is unmeasured, so the gate deliberately encodes none — marker or tail confirms
at any size. Codex 0.160.0 showed the same shape in a partial 79x57 frame (a
~600-character paste drawn inline with no marker), so Codex takes the same
tail rule; a collapsed draft carries no visible tail, so only its marker can
confirm it. The Codex frame in the regression is synthetic (full draft text in
redraw chrome, no marker) because a clean live capture was blocked by a
hooks-review dialog — it pins the rendering shape the rule relies on, and a
byte-exact live capture is still outstanding. For either harness, a draft past
the full-text limit (measured after
normalization, like the matcher) is therefore also confirmed by its last 64
letters and digits appearing in output received after the write. The paste is
read in order, so the tail appearing means the text before it was accepted, and
a scrolling composer keeps the tail in view. Run 343's 831-character `publish`
prompt was visibly staged but waited the full budget for a marker Muse never
prints, so the step ended Unverified with no Enter sent. Known limit: a stale
redraw that repaints an earlier prompt with an identical ending could satisfy the
tail match; the one-second quiet requirement narrows this but does not remove
it (pinned as a regression test so a future token or composer-region hardening
visibly flips it).

A captured Codex 0.160.0 Windows ConPTY redraw at
22 columns inserted extra spaces inside `[Pasted Content 9407 chars]`; the
previous literal matcher rejected it. The tail anchor survives the same
fragmentation: normalization drops whitespace, so ConPTY padding and line
breaks inside the tail region cannot hide it (covered by a narrow-width
regression). Run 320's ledger retained prompt intent
and timed out after 30 seconds; its Codex transcript later contained the full
9,407-character feedback prompt. The historical paste screen was not retained,
so that record alone cannot establish its exact rendering.

Delivery rechecks readiness every 250 ms within one 90-second total budget,
with progress logs at 30 and 60 seconds and elapsed time in the exhaustion error.
The budget allows two extra original 30-second intervals for a slow console
reader or redraw. Guarded delivery blocks the shared circuit dispatcher during
this wait, delaying other effects and the Unverified checkpoint by up to 90
seconds; that bounded delay is accepted to recover an already staged paste
without risking a duplicate prompt. Separate Enter acknowledgement can then
take up to three six-second attempts.

Readiness checks observe the existing draft without writing it again. A complete
echo is retained while redraws settle, even if later output evicts the marker
from the bounded tail. A changed input ownership stamp or a closed process ends
the wait. Quiet output without a complete matching echo cannot authorize Enter.
Once ready, the existing bounded Enter acknowledgement retry applies. Exhaustion still
records an Unverified checkpoint; restart never repeats the uncertain prompt.

The delivery regressions replay the captured ANSI fragment through the real
evaluator, reject stale, incomplete and wrong-count markers, admit a late echo,
and verify that readiness retries write no input; they additionally cover the
Codex mid-size tail confirmation (full, partial and wrong-ending frames), the
per-adapter policy declaration, the stale-redraw limit, narrow-width tail
fragmentation, and size-independent tail anchoring. These are module-boundary
checks with a capturing process registry. The live ConPTY capture establishes
the provider rendering; it does not establish a rebuilt Circuit run end to end.

Checkpoint outcomes require the history revision, run state, step attempt, and
effect kind observed by the operator. A stale operator result or late worker
result cannot replace cancellation or a newer attempt. Only OpenPr has a
read-only external recheck. Other GitHub mutations and prompt/spawn outcomes
remain manual attestations; `not_performed` is the only path to a deliberate
new attempt.

OpenPr checkpoints with a saved target also receive up to five automatic
read-only rechecks, at least one minute apart. The count and next eligible time
are persisted per attempt before lookup; restart cannot reset the allowance.
Manual recheck remains available afterwards. Neither path reclaims the original
mutation. A matching lookup commits the PR identity and reconciliation together.

Attached-agent completion gates can accept an operator-recorded completion,
using the same eligibility policy for the displayed action and the command.
The transaction checks the history revision, current run and attempt, and known
work/request/conflict blockers, then records the outcome without synthesizing
native observations. Review verdicts and approval gates cannot be attested this
way. An already-admitted feedback delivery can be attested on a changes-requested
route; requiring all ancestor verdicts to be approved would deadlock that route.
The next ordinary worker tick schedules downstream work.

The focused deterministic evidence is:

- `worker_effect_persistence_mapping_covers_every_effect_variant` sends each
  stepper effect through the worker persistence mapper and checks its journal,
  atomic-status, or separate-policy route.
- `effect_claim_survives_reopen_and_is_never_replayed` checks that an
  unclaimed intent remains claimable after restart and that the resulting
  possible-dispatch claim cannot be claimed again for spawn, prompt, or GitHub.
  That persisted state also covers a remote success whose acknowledgement was
  interrupted: restart cannot distinguish it from a lost response, so neither
  result permits automatic replay.
- `unsupported_github_mutations_stay_uncertain_without_replay_on_recovery`
  drives AddLabel, RemoveLabel, PostComment, and CloseIssue through the worker
  recheck path; each remains Unverified and does not dispatch a second mutation.
- `spawn_attachment_acknowledgement_is_atomic_fenced_and_deduplicated` covers
  cancellation, attachment acknowledgement, and injected history failure.
- `github_recovery_tests` drives OpenPr create, restart lookup, absent-result,
  and cancellation races through the worker handoff against a loopback GitHub
  endpoint. The endpoint and process stop are simulated; a live process crash
  and live GitHub mutation are not claimed.
- `set_node_status_and_step_commit_atomically_across_restart` injects a
  history-write failure to prove the status and step roll back together, then
  reopens the database and checks the committed pair.
- `unavailable_set_node_status_target_fails_run_without_retrying_forever`
  drives missing and archived targets through the worker's fenced failure
  commit; `set_node_status_does_not_apply_after_circuit_run_is_cancelled_or_deleted`
  proves a terminal or deleted run cannot receive a late status mutation.
- `close_agent_retry_is_not_observed_after_close_clears_spawn_association`
  and `walking_skeleton_close_retry_observer_emits_nothing` pin close replay
  and its stop condition.
- `circuit_recovery_history_survives_reopen_and_matches_successor_context`
  injects a continuation-history failure, checks transaction rollback,
  reopens the database, repeats the request, and verifies successor reuse and
  the absence of implementation/publication steps or effect claims.
- Database checkpoint tests cover competing and stale operator revisions;
  OpenPr tests cover a late result after cancellation.

Run the focused Circuit Rust tests serially because most worker integration
fixtures share one process database. The per-test `db::circuit` unit tests use
private connections and remain parallel-safe.
