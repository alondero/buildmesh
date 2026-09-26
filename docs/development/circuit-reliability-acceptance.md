# Circuit reliability acceptance

Implementation evidence for [#1889](https://github.com/alondero/buildmesh/issues/1889), governed by [#1850](https://github.com/alondero/buildmesh/issues/1850). This is a working acceptance record, not a claim that the specification is implemented. Baseline: `d8e3a1a780599cbdcece4710df2b603860065e7a`.

Source inspection, deterministic automated checks, and live delivery are separate evidence tiers. No aggregate reliability percentage is asserted. Unless recorded otherwise below, platform evidence is Windows, development configuration; harness versions and live delivery are recorded per scenario.

| Scenario / stories | Contract and invariant | Owning seam / automated layer | Evidence and result | Remaining gap |
| --- | --- | --- | --- | --- |
| Lost/delayed hooks, missing optional tokens, stale/late observations (1-8, 14) | #1845: identity fences, stable deduplication, bounded reconciliation | `observation`, native receipts, Codex observer; pure and private DB tests | Windows deterministic tests pass; native request receipt/commit/reopen and delayed start/stop regressions exercised. Codex 0.156.1 / Luna live foreground pull and SessionStart, UserPromptSubmit, Stop callback delivery are recorded below | Live question request/reply and a no-ID permission receipt are recorded below; authoritative permission resolution and delayed-hook recovery remain unverified. Claude production submission correlation is unavailable; other harness strategies remain explicitly unsupported |
| Child/background work (9, 11-13) | #1844: foreground termination and all owned work must be terminal | `WorkEvidence`, atomic observation batches; pure tests | Windows scripted ownership tests pass, including late child termination and missing registry entries | No available live harness establishes complete owned-work coverage. Codex live result stays Unverified |
| Human waits (10, 15, 21) | #1846: human waits do not expire or grant authorization | Typed wait observations, stepper, history UI | Windows regression reproduced generic Working incorrectly clearing permission; typed identity/request matching and UI tests added | Claude/Codex exact-request callbacks are wired; uncorrelated waits remain open with an explicit limitation. Live question correlation passed below; permission resolution and human-input-only progression gating remain Unverified |
| Restart and cancellation (22-23) | #1846: terminal cancellation and identity-proven reattachment | Ledger, worker restart, process registry; serial Rust tests | Windows tests pass for cancelled-run fences, ambiguous spawn recovery, receipt reopen, and process generations. Current rebuilt app cancellation left Codex run 45 terminal at attempt one with history retained | Actual app restart resumed the saved Codex session and retained evidence (below); the pending question was interrupted rather than restored, so full reattachment acceptance remains Unverified |
| Unknown effects and recovery races (16-21) | #1846: uncertainty never authorizes replay | Stepper, transactional journal, worker dispatch; serial Rust tests | GitHub, prompt and spawn claims survive reopen. Attachment acknowledgement is atomic; injected history failure rolls it back. Competing operator revisions are fenced. OpenPr recheck uses only the saved owner/repo/head lookup; deterministic tests cover a matching PR result, `NotPerformed` then a match, and cancellation before the late result commits | Live read-only OpenPr recovery passed in the continued acceptance checks below, including a retained NotPerformed attestation. Actual create-crash and live cancellation races remain unverified; other effect kinds and continuation still need a complete typed journal audit |
| Operator history and outcomes (24-28) | #1847: append-only causal trace and actionable uncertainty | Ledger, IPC, rendered Probe; Rust and Vitest | Typed observation/classification provenance, effect history, scrubbed complete/partial/unavailable reports, operator reasons and capabilities exercised. Current rebuilt WebView2 shows Codex's ownership limitation and same-attempt Recheck | Wait/capacity/configuration history completeness remains under audit; current live viewport was not the 240px Probe check |
| Review snapshots/continuation (29-32) | #1848: frozen graph/configuration and successor deduplication | Review ledger, launch capture, blueprint UI; Rust/Vitest/live IPC | Frozen effective launch configuration and graph tests pass. Live blueprint copy was disabled/manual/independent; original mutation rejected. Current-build live copy, disable, concurrent continuation, lineage and cancellation checks pass; the continuation link is now durable in the run's own history | The live reviewer dispatch is Unverified: the borrowed-source gate parks Unverified on this harness (see the #1910 section). A run on a copied Review Blueprint offers no Continue review control, so continuation of a *first* review on a copy has no UI entry point. Blueprint mutation refusal is deterministic-only (the read-only surface has no control to click) |
| Legacy retirement/capacity (33-36) | #1849: retained history, no conversion/restart, separate capacity | Startup retirement, spawn/borrow claims, generation-fenced teardown, retained-settings UI | Windows deterministic cutover/reopen tests pass; no Circuit conversion or capacity transfer. Current source build passed real dev IPC, retained-node, 240px, and reload checks | Startup cutover was covered; a forced process crash during cleanup and cleanup retry remain untested |
| Classification (38-40) | #1850: exact fresh report interpretation cannot prove lifecycle | Immutable report envelope and pure classifier gate | Windows regression suites pass for wrong session/attempt/report/input, delayed children and invalidated evidence. Complete report retained separately from interpretation | All-harness report coverage remains incomplete. Optional synthetic fast-model comparison deferred |


Every supported observation strategy needs recorded capabilities, source, freshness bounds, child/background coverage, degraded cases, harness version, platform and launch/trust configuration. Fixture parsing alone cannot establish live delivery. Unsupported or untested combinations remain unverified.

## Continued acceptance: 2026-09-26: Review Blueprint copy, deletion and continuation

Work for [#1910](https://github.com/alondero/buildmesh/issues/1910). This section
closes the two items the row above left open — the current-build continuation
check and the blueprint availability/deletion audit — and records one behaviour
that was missing: a Review Successor now names the run it continues in its own
append-only Circuit Run History, not only in the run context that retention
later empties.

Environment: Windows, development profile, Codex CLI 0.157.0 (`codex --version`;
its own TUI banner reads v0.157.1) on `gpt-6-luna` in native Windows PowerShell.
Pre-launch log counts were `buildmesh.log` 51584, `panic.log` 132,
`panic_early.log` 48; the panic files did not grow and the stable hub was never
stopped. The only new `ERROR` lines are the pre-existing
`resize_agent: Agent not running` noise and health probes for a long-deleted
#1889 fixture directory.

### What the live run did

An isolated Mesh on a scratch repository, one real Codex implementation turn
delivered through the production `write_to_agent` command body, then a
two-generation failed lineage built by the new bridge fixture through
production seams only (`list_circuit_probe`'s blueprint ensure,
`copy_review_blueprint`, `create_node_circuit_run`, `continue_failed_review`,
`commit_circuit_advance`). Everything after that was the real app: Probe
buttons, canvas editor, the circuit worker, and durable SQLite rows read back
afterwards.

| Check | Observed result | Evidence boundary |
| --- | --- | --- |
| Built-in Review Blueprint availability | Present, `is_preset = 1`, `enabled = 0`, matching the local review contract; Probe row titled "Review Blueprint" with **only** "Inspect Review Blueprint" — no enable, trigger or delete control | Real IPC `list_circuit_probe` and rendered Probe. The absence of the controls *is* the read-only proof; the refusal messages themselves are deterministic-only |
| Blueprint inspector | "Read-only Review Blueprint", Copy control present, no Save, no palette | Real canvas editor. `1910-blueprint-read-only.png` |
| Copy through the UI | Real `copy_review_blueprint` minted `Review Blueprint copy` as `is_preset = 0`, `enabled = 0`, all entry points `manual`, `concurrency_limit` 2 inherited, zero runs; the blueprint's `graph_json` stayed byte-identical | Real IPC. `1910-blueprint-copy.png` |
| Copy is editable and independent | Changing the reviewer prompt in the canvas and saving persisted to the copy; the blueprint's `graph_json` stayed byte-identical | Real `update_circuit_graph` |
| Disabling the source Circuit | `set_circuit_enabled(copy, false)` through the Probe checkbox; the follow-up still continued from the retained run's pinned graph and frozen launch plan | Real IPC. The pinned-scope assertions are the same ones the deterministic test makes after a delete |
| Concurrent continuation | Two genuinely overlapping calls into the production `continue_circuit_review` body returned the **same** successor and created exactly one run row | The Probe serialises its Continue button behind one busy flag, so the wire race is driven by a bridge command that calls the same command body from two threads — production code, not the UI |
| Lineage | The live successor's `recovery.from_run_id` is the **failed first successor**, not the original run, and its own `review_continuation` history entry names that run | Durable rows after the real call |
| Pinned scope | The successor's pinned snapshot contains no `github_action` and no `implementer`; `reviewer` is the only `spawn_agent_node`; its prompt is the retained run's, not the copy's post-hoc edit | Durable snapshot read back |
| Repeated real-IPC continuation | A second Continue review from the failed successor's card minted nothing (run count unchanged) | Real IPC |
| Effect journal and history | The follow-up claimed **no** effect rows at all; its history is `configuration_pinned`, `review_continuation`, `run_transition`, three `step_transition` entries, one `observation` and one `classification` — no GitHub action and no `open_pr` node | Durable rows |
| Ownership | The follow-up has no step owning the implementation agent, and the Mesh contains only the source agent | Durable rows |
| Cancellation | Real Probe cancel left the follow-up `cancelled` with 8 history entries retained and the ancestor still `failed`; two further continuations of the ancestor were both refused with "The continued review was cancelled. Start a fresh review." and minted nothing | Real IPC |
| Reviewer dispatch | **Unverified.** The borrowed-source `await_source` gate parked Unverified ("The report was interpreted, but foreground or owned-work completion remains unverified"), a fresh source turn inside the follow-up window did not clear it, and a reasoned Recheck through `record_circuit_outcome` did not either. The reviewer was never dispatched | This is the parent issue's open Codex owned-work/foreground-termination gap, reproduced in a new place. The bound on *what may be dispatched* is asserted deterministically instead: a full review round emits a `SpawnAgentNode` only for `reviewer` and never a `CallGithub` |
| Cleanup | The fixture Mesh, its runs, circuits, agents and scratch repository were removed; the dev profile is back to its prior fixture set | Bridge teardown |

### Findings that are not defects in this issue

- **A run on a copied Review Blueprint has no Continue review control.** The
  Probe's `canContinueReview` recognises only the built-in preset
  (`source.review_preset`), an earlier continuation
  (`recovery.from_run_id`) or the issue-driven blueprint. A Review-derived
  Circuit's run gets none of those, so the affordance is hidden — even though
  the backend will continue it and the deterministic suite proves it
  (`copied_review_continuation_requires_the_frozen_review_contract`). #1848
  owns that policy, and #1910's scope is "continuation of an existing review
  successor", so this is recorded rather than changed.
- **Every continuation mints a visible "Continued review" Circuit.** The
  recovery Circuit is `is_preset = 0`, so it appears in the user's Circuit list
  next to their own. Reuse is keyed on the frozen graph, so one Mesh shows at
  most one per distinct frozen scope. It is existing behaviour and is visible in
  the History screenshot.

### Added deterministic coverage

- `review_blueprint_copy_is_authorized_only_from_the_built_in_and_survives_source_disable`
  — a blank name and a user Circuit are both refused as copy sources; then
  disabling, rewriting **and deleting** the Circuit the run was started from
  leaves the retained run's pinned reviewer scope and frozen launch plan intact
  and continuable.
- `continued_review_dispatches_only_review_work_and_records_the_run_it_continues`
  — a failed issue-driven run that owned an `implementer` step and an `open_pr`
  step yields a follow-up with neither; the successor claims no effect, borrows
  rather than owns the implementation agent, records its parent in its own
  history, leaves the ancestor's history byte-identical, and a cancelled
  successor closes the lineage. This test failed before the fix.
- `a_full_review_loop_dispatches_only_the_reviewer` — across findings, feedback
  to the borrowed source, a fresh reviewer and approval, the only spawned node
  is `reviewer` and no `CallGithub` effect is ever emitted.
- `the_built_in_review_blueprint_cannot_be_triggered_on_its_own` — Trigger Now
  on the built-in Review Blueprint is refused and leaves no run, so an
  unattended row can never mint a run.
- Vitest: the `review_continuation` history entry renders as "Continued a
  failed review" with the run id it names.

The clean checkout and GitHub state were checked before this work: PR #1893 was
open, draft and mergeable; issue #1889 was open. The earlier implementation and
full Windows gate remain approved. Historical checkpoints below describe their
own snapshots; the results here describe additional work, not full acceptance.

### Live OpenPr recovery

`npm run tauri:build:dev`, with `CARGO_TARGET_DIR` set to
`src-tauri/target/release-dev`, passed on Windows. The resulting dev binary was
launched with WebView2 CDP on port 9223. The stable app was not stopped.

The controlled fixture seeded a durable uncertain OpenPr effect with saved target
`alondero/buildmesh`, branch `tingly-watered-lynx`. No original create request was
dispatched. Real Tauri IPC `record_circuit_outcome` requested Recheck; the real
worker and GitHub client performed the read-only lookup and committed its result.

| Command / controlled scenario | Observed result | Evidence boundary |
| --- | --- | --- |
| `node scripts/ui-shot.mjs --out .tmp/1889-continued-openpr.png --steps .tmp/1889-continued-openpr.mjs` | Passed: Run 47 / Circuit 47, PR 1893, attempt 1, completed run and step, acknowledged effect, 10 history entries | Live GitHub lookup through worker and durable commit; initial uncertainty was seeded |
| Same command with `BUILDMESH_ACCEPTANCE_NOT_PERFORMED=1` and output `1889-continued-openpr-attested.png` | Passed: Run 48 / Circuit 48, PR 1893, attempt 1, acknowledged effect, 12 history entries; NotPerformed attestation retained | Actual operator IPC followed by live lookup; no deliberate retry or create |
| Final source `7d97617`: `npm run tauri:build:dev`, then `node scripts/ui-shot.mjs --out .tmp/1889-final-openpr.png --steps .tmp/1889-continued-openpr.mjs` | Build passed; Run 51 / Circuit 51 passed, PR 1893, attempt 1, completed run and step, acknowledged effect, 10 entries | Same controlled real lookup and worker handoff on the committed implementation |
| Final source, same script with `BUILDMESH_ACCEPTANCE_NOT_PERFORMED=1`, output `1889-final-openpr-attested.png` | Run 52 / Circuit 52 passed, PR 1893, attempt 1, acknowledged effect, 12 entries; attestation retained | Same no-create fixture and real operator IPC on final source |

All four runs asserted zero new `effect_possible_dispatch` entries and retained the
exact PR number and branch in durable context. Scratch JSON evidence is in
`.tmp/1889-continued-openpr-evidence.json` and
`.tmp/1889-continued-openpr-attested-evidence.json` (final-source runs; baseline
copies use `1889-baseline-openpr` names). Final build log:
`.tmp/1889-continued-final-dev-build.log`. This establishes the recovery
lookup-to-worker handoff. It does not establish a real crash during PR creation,
a live cancellation race, or the Codex stale-completion race.

### Permission callbacks without request identity

The Codex native adapter now retains a `PermissionRequest` without a request ID
as a reduced-confidence, uncorrelated permission wait. Previously it discarded
the callback and could expose only a generic status-derived input wait. No
request ID is invented, and an unrelated tool result cannot resolve the wait.
The regression covers normalization, unrelated and wrong-session replies,
serialized evidence restoration, and subsequent input/ready/working projections.
The live no-ID PermissionRequest recorded below also exercises callback retention; exact permission request/reply resolution remains Unverified.

### Current Windows Codex foreground observation

Codex CLI 0.157.0 / `gpt-6-luna`, native Windows PowerShell, ran a fresh
synthetic no-tools prompt through the real dev app (Mesh 45, Circuit 49,
Run 49, Agent Node 128). Project hooks were enabled; the adapter launched
with approval `never`, sandbox `danger-full-access`, and hook-trust bypass.
This configuration does not establish approval-prompt delivery.

The exact session `01a0d80f-8139-7861-b94a-30f8aa468f78` and turn
`01a0d80f-85a7-73e3-a2a8-7da1482ae336` supplied authoritative foreground
termination through `codex_rollout_task_complete`. The same batch recorded
owned-work coverage as unavailable. The step remained Unverified at attempt 1.
Real IPC Recheck recorded the existing facts as duplicates; the rollout still
contained exactly one task start, one task completion and one controlled prompt.
No prompt or spawn replay was observed. There were no human-request native
receipts; this run proves rollout observation, not human-hook delivery.

Commands used `node scripts/ui-shot.mjs --steps` with
`.tmp/1889-continued-start.mjs` and `.tmp/1889-continued-recheck.mjs`, followed by
`node .tmp/1889-continued-counts.mjs`. Durable state, session identity and rollout
counts were recorded in `.tmp/1889-continued-result.json`. Later invocations of
the counts script updated that scratch file with cumulative multi-turn counts;
the one-turn result above describes the checkpoint before the question and
permission experiments, not the final contents of that file.

### Live question request and reply

On the baseline dev binary (`25308d33`), the same Run 49 / attempt 1 / Agent 128
was explicitly switched to Codex Plan mode. The real `request_user_input` tool
asked "Which color should I use?" with Blue and Green options. The unanswered
question was inspected in the terminal before Blue was submitted.

The native PreToolUse receipt (history 485) and accepted request observation
(486) retained request ID `call_NQ5egkF8ECx6NArJwjB4alyt`. PostToolUse receipt
487 and accepted response observation 488 retained that exact ID, session
`01a0d80f-8139-7861-b94a-30f8aa468f78`, incarnation `1790331283070`, and turn
`01a0d824-a0bb-7fb3-b3bf-98ea5f30d986`. The rollout's matching function output
contains the Blue answer. This establishes live question callback delivery and
request/reply correlation on native Windows Codex 0.157.0 / Luna.

The run was already Unverified for unavailable ownership coverage before this
question. Its continued lack of progression does not independently prove that
an otherwise runnable Circuit is blocked solely by human input. That acceptance
criterion and live permission resolution remain Unverified. Native receipts
were turn-fenced but did not establish
Buildmesh submission correlation.

Scratch evidence: `.tmp/1889-continued-human-wait.json`, with inspected captures
`1889-continued-question-wait.png` and `1889-continued-question-answered.png`.
Commands used the controlled Plan-mode input and answer scripts through real
Tauri `write_to_agent` IPC. A separate Run 50 / Agent 129 attempt displayed the
Codex welcome screen but never supplied a captured session or readable report;
it is not counted as a passed request flow.

### Actual app restart and remaining wait limitation

The controlled dev watchdog (PID 77188) was stopped before its parent app
(74412), leaving the stable app untouched. The committed implementation
`7d97617` was built and launched with CDP 9223 (app 74068, watchdog 60212).
Startup logged `resumed node 128`; a new `codex.exe resume` process (4916)
started with the same saved session ID. Run 49 retained Agent 128, attempt 1,
its persisted incarnation and question/reply observations. One spawn intent and
one acknowledgement remained; the step stayed Unverified rather than completing.

Timestamp inspection corrected the initial visual assumption that a second
question prompt had remained unsent: a third turn and question request
`call_0v980n2XoaOQ00yWKAE7SmkX` were recorded before shutdown (history 489/490).
After restart the real terminal showed an interrupted conversation and no
answerable Yes/No request. No answer was injected into that missing request.
The displayed prompt contains text repeated during the controlled submission
attempts; this alone is not evidence of worker replay.

This establishes session resume and durable evidence retention across an actual
app restart. It does **not** satisfy pending-human-wait restoration or complete
work reattachment acceptance: the interrupted question was not resolved after
restart. Evidence: `.tmp/1889-continued-restart-check.mjs`,
`.tmp/1889-continued-restart-result.json` and the inspected
`.tmp/1889-continued-resumed-agent128.png` capture.

### Live permission attempt

On the final dev binary, the resumed Agent 128 exposed the real `/permissions`
menu. Selecting Ask for approval displayed `Permission selection requested:
Ask for approval`. A harmless scratch-file creation then executed without a
visible approval prompt. Its PostToolUse receipt (history 513) carried request
ID `exec-ca309c27-9f30-4572-a160-30aa66dfca00`; the corresponding permission
response observation (514) was rejected without a matching permission request.
An ordinary workspace write need not require approval, so neither the menu
selection nor this result establishes a permission request/reply round trip.
No approval was inferred from the unmatched response.

A subsequent explicit approval-required probe requested only
`Write-Output BUILDMESH_PERMISSION_CHECK` in the disposable workspace. This
produced a real `PermissionRequest` (history 515) with session
`01a0d80f-8139-7861-b94a-30f8aa468f78`, turn
`01a0d83d-a2c4-72b3-bcbf-cf23c5aad469`, and no request ID. History 516 retained
`permission_requested` with reduced confidence, and Agent 128 became
`awaiting_input`. This is live evidence for the new no-ID callback retention;
it is not authoritative permission resolution.

The actual terminal displayed Yes, proceed; a persistent allow option; and No.
Only the one-time approval was selected. The terminal then showed the exact
command and `BUILDMESH_PERMISSION_CHECK` output. During the bounded inspection,
history still ended at the uncorrelated permission request, without a matching
response hook. Thus the live harness approval UI and no-ID request retention
were exercised, but authoritative request/reply resolution remains Unverified.
Inspected captures: `.tmp/1889-continued-permission-prompt.png` and
`.tmp/1889-continued-permission-approved.png`. No persistent command permission
was granted.

### Fixture cleanup

Real Tauri IPC cancelled Runs 49 and 50 and disabled Circuits 49 and 50.
Their histories remained present: 38 entries for Run 49 (last ID 516), 12 for
Run 50 (last ID 484). The final permission response hook had not arrived by
cleanup. The owned dev watchdog 60212 was stopped before app 74068; both were
confirmed absent. The stable app was untouched. Completed OpenPr fixtures were
already disabled and retained their completed run histories.

### Additional automated evidence

| Command | Executed result | Scope / limitation |
| --- | --- | --- |
| `scripts\check.ps1 all -SerialRust` (final rerun) | Passed: 3,508 unit tests / 1 skipped; 69 TypeScript integration tests; 3,856 Rust library tests / 24 ignored; 18 Rust integration tests | Full Windows gate also passed agent/readme/lint-fixture checks, docs, desktop/mobile builds, lint and bundle budget. One Rust doc-test remains ignored. Log: `.tmp/1889-continued-full-gate-final.log` |
| From `src-tauri`: `cargo test --locked --manifest-path Cargo.toml services::circuit_worker::github_recovery_tests -- --test-threads=1` | 3 passed, 0 failed, 3,877 filtered | Injected lookup through production worker outcome preparation and commit: successful reconciliation, missing/mismatched lookup, blocked lookup cancelled before commit; fresh DB connection reads and no second effect claim. This is not a process restart or live GitHub race |
| From `src-tauri`: `cargo clippy --locked --all-targets` | Exit 0; 2 library warnings and 28 library-test warnings (1 duplicate); no warning in files changed by this continuation | Warning attribution outside the touched files was not rechecked at the baseline; this is not a warning-free whole-repository result |
| `npm run check:agent -- --base 524a65278d6cc8edaac0e71c5fec33a4f755cbde` | Passed | Correct branch merge base; includes approved prior work and continuation changes |
| `npm run check:docs` | Passed, 120 Markdown files | Documentation contract |

The first redirected gate invocation stopped on PowerShell treating Node's
`NO_COLOR`/`FORCE_COLOR` stderr warning as an error, before test execution. The
next `scripts\check.ps1 all -SerialRust` invocation reached every gate: Rust
passed 3,856 library tests (24 ignored) and 18 integration tests; TypeScript
integration passed 69. Its unit suite failed one Project Files panel text wait
(3,507 passed, 1 failed, 1 skipped). The isolated
`npx vitest run tests/unit/probe-panel.test.tsx --pool=threads` rerun passed all
20 tests with `NODE_ENV=test`; this alone does not turn the failed full gate green.
No frontend source or test was changed in response to that failure.
The subsequent full rerun passed, including all 20 tests in the affected file;
the earlier failure remains recorded above. The initial focused permission test
attempt hit a shared-executable linker lock and executed no tests; the final
full Rust suite executed and passed the new permission regression.

### Platform and strategy limits

| Boundary inspected | Actual environment / source | Acceptance status |
| --- | --- | --- |
| Native Windows Codex rollout pull | CLI 0.157.0, ChatGPT login, Luna; Run 49 above | Foreground and non-replaying recheck observed; complete ownership Unverified |
| Native Windows Codex request hooks | Configured PreToolUse/PostToolUse and PermissionRequest callbacks | Question request/reply and no-ID permission request observed below; authoritative permission resolution remains Unverified. The no-tools run establishes neither |
| Windows host / Ubuntu WSL2 | Linux 6.6.87.2-microsoft-standard-WSL2 x86_64; `/usr/bin/codex` 0.49.0 | Below the adapter's 0.154.0 hook minimum; current hook contract Unverified. Ordinary startup rejects configured effort `xhigh`; `codex -c model_reasoning_effort=low login status` confirms ChatGPT login without changing configuration. No WSL model was launched |
| Claude native receipts | Windows CLI 2.1.282; user has no Claude account | Live delivery Unverified; authoritative submission correlation remains unavailable |
| Other installed Windows harnesses | OpenCode 1.18.3, Kimi 0.27.0, Agy 1.2.11, Cline 3.0.62, Grok 1.0.41 | Installation does not establish a Circuit lifecycle/ownership adapter. Current observer policy marks these unsupported; no completion claim |
| Native Linux and macOS | No corresponding host exercised | Unverified |

Inventory used `wsl --list --quiet`, command/version probes and login-status
commands only. Authentication material was not read or recorded. Unsupported
boundaries are gaps, not passed completion scenarios.

## Implementation checkpoint: 2026-09-24

This work is not ready to close #1889. The typed ownership core now receives
Claude native hook receipts and Codex foreground-completion pulls. Claude
delivery has no live evidence here because the installed account is unavailable.
Codex rollouts do not establish complete child/background coverage; the adapter
keeps that limit visible as Unverified. Production status snapshots enter with
reduced confidence. Classifier routing now requires an exact retained native
report, lifecycle coverage and an immutable input/session fence. The initial
Ready-only review handoff has been removed. Claude hook submission correlation
is unavailable: native turn IDs do not acknowledge Buildmesh input, so receipt
arrival cannot authorize completion. Those receipts remain reduced confidence.

Implemented and exercised so far:

- Classification history records interpretation separately from lifecycle proof,
  retains report text, and labels unverified completeness as partial or unavailable.
  Regression sequences cover delayed child receipts after new input, invalidated
  lifecycle followed by status projection, and delayed native start/stop receipts.
  Report stamps and timestamps cannot be refreshed by unrelated observations.
- Current harness capabilities are exposed per attached step with host/launch
  environment and evidence deadline. Codex uses a 60-second yielded reconciliation
  window; Claude receipts use 90 seconds; unsupported adapters use 30 seconds.
  Active-work and explicitly authored budgets remain separate; human waits do not
  expire. Unknown harnesses do not inherit another harness's observation policy.

- GitHub and prompt intent and possible-dispatch claims commit durably; repeated claims do
  not dispatch again after database reopen. Missing results preserve the attempt
  as Unverified. Operator outcomes require a reason, keep permission/review gates
  separate, and fence competing actions by history revision. Not-performed and
  deliberate retry are separate actions.
- Observation records, projection/context, and effect intent share one
  transaction. An injected history append failure rolls them back together.
  Pure transition tests cover active child work after foreground termination,
  missing tokens, duplicate/stale/conflicting evidence, and terminal cancellation.
- Native hook receipt batches preserve foreground, report, and ownership facts
  together. A child that disappears from a registry remains owned until explicit
  terminal evidence arrives. Malformed receipts and receipts for deleted agents
  are recorded as rejected without blocking later receipts. Current input and
  persisted session incarnation are checked again at the commit boundary.
- Evidence recheck reactivates the same attempt with continuation disabled.
  Input waits do not expire. The stale-agent sweep excludes current Circuit
  ownership under both its reader and writer checks.
- New runs pin graph JSON and a behavior revision. Graph edits cannot change
  active execution or recovery. Reviewer launch plans now capture effective
  model, effort, arguments, harness runtime and provider route at creation;
  continuation carries the snapshot. Node-started, manual and issue-triggered
  creation are covered. A retained source snapshot survives removal of its
  custom harness profile; current credentials remain necessary at launch.
- Probe and editor run history use the registered backend commands. Windows
  WebView2/CDP checks rendered backend fixture history at a 240 CSS-pixel Probe
  width. A synthetic unknown-action fixture proved required reasons, visible
  attestation, a separate retry offer, and stale IPC action rejection. No GitHub
  action was sent by the smoke. Screenshots and scripts are in ignored `.tmp`.

Verification results at this checkpoint:

| Check | Result | Limits |
| --- | --- | --- |
| TypeScript and ESLint | Passed | Before subsequent changes; rerun before commit |
| Focused Rust Circuit run | 472 passed | Includes frozen launch configuration, source snapshot inheritance, trigger deduplication, blueprint copy and continuation contract |
| Attention route tests | 86 passed | Before latest receipt isolation changes |
| Focused Circuit UI tests | 96 passed | Three files; full responses, Unverified warnings, and read-only/copyable blueprint |
| Clippy library gate | Failed on two existing findings | Both reproduced at base: empty doc-comment line in `environment.rs` and unnecessary string allocation in `harness_catalog.rs` |
| Full Rust library suite | 3,763 passed, 24 ignored | Includes blueprint copy and frozen configuration; rerun after subsequent work |
| Full Vitest run | 3,558 passed, one failed, one skipped | Radius audit failure at `AppSettings/UsageRender.tsx:308` also reproduces at base `d8e3a1a7` (3 passed, one failed) |
| Windows dev build | Passed and launched | Later backend edits require a rebuild |
| Real IPC / narrow UI | Passed for synthetic evidence, live Luna checkpoint/recheck, and blueprint copy | Separate runtime scopes; no Claude/WSL/Linux/macOS delivery claim |

Installed Windows harness inventory: Claude Code 2.1.281, Codex CLI 0.156.1,
OpenCode 1.18.3, Kimi 0.27.0. Claude's isolated smoke failed with expired OAuth;
the operator requested another harness and selected Codex Luna. Codex 0.156.1
with `gpt-6-luna`, Windows PowerShell, and a read-only sandbox completed a
synthetic response and a controlled 15-second shell command. CLI JSON recorded
the command in progress, then completed with exit zero, then the final response
and `turn.completed`. Its persisted rollout recorded matching `task_started`,
`CommandExecution` completion, and `task_complete` turn identities and ordinals.
This CLI evidence does not prove complete owned-work coverage. A subsequent
real Windows development Circuit launched Codex `gpt-6-luna` with a synthetic
no-tools prompt (run 40, agent 120). The rollout's `turn_context` confirmed the
model and `task_complete` contained `BUILDMESH_LUNA_CIRCUIT_OK`. Buildmesh
recorded authoritative foreground termination for that same session and turn,
recorded ownership coverage as unavailable, and kept attempt one Unverified.
Real Tauri IPC and inspected WebView2 screenshots confirmed the explanation and
Recheck-only action. Cancellation left both this run and the separate synthetic
effect checkpoint terminal with history retained. This exercised the native pull
strategy on Windows. A fresh run (41, agent 121) exercised real IPC Recheck:
  duplicate native evidence returned the same attempt to Unverified without
  prompt replay. Its readable final-response history and 240px controls were
  inspected, then the owned smoke run was cancelled. WSL, Linux, and macOS
  have no runtime evidence here. The retained rollout confirms model
`gpt-6-luna` and one occurrence of the smoke prompt. It also records a later
turn whose input is a terminal OSC colour-query response; this is not a repeated
smoke prompt, but the cause of that unexpected input remains undiagnosed.

A later Windows dev build exercised **Inspect Review Blueprint**, rejected a
preset graph mutation through real IPC, and copied it through the UI. The new
Circuit was editable, disabled, manual, on the same Mesh, and had no transferred
runs; the original graph was unchanged. Both screenshots were inspected. All
owned smoke runs were cancelled and fixture Mesh 43 (including copied Circuits)
was removed through the fixture cleanup bridge. No new panic log entries
appeared; synthetic retained agents produced `resize_agent: Agent not running`
errors while switching views.

Remaining acceptance work includes per-harness authority/capability adapters,
persisted live ownership, distinct permission/question/input projections,
full per-strategy reconciliation and live delivery, all prompt/spawn effect boundaries, complete
scrubbed native review reports for all harnesses, effective configuration
history presentation, live legacy-cutover verification, and the full scenario/live matrix. Review-generation
deduplication now follows failed generations, reuses active/successful successors,
and refuses to bypass a cancelled successor. Those lineage regressions pass in the focused and full Circuit checks.


Latest focused evidence: 484 Circuit Rust tests passed before the final report
completeness additions; 98 UI tests passed across history, editor and Probe.
These are automated fixture checks, not a new live build. The earlier full Rust
suite passed 3,763 tests with 24 ignored; a final full-suite run is still required.


Legacy cutover now has a durable cancellation/stop ledger, startup ordering before
auto-resume, terminal legacy cancellation, blocked enable commands and a read-only
retained-settings surface. Its DB regression preserves nodes, PR history and both
capacity settings across reopen; it creates no Circuit. Sixty focused UI tests
passed after replacing obsolete legacy activation tests with cutover, retained
settings, independent capacity, failed saves and stale-Mesh read tests. Live
cutover and final full suites remain pending.

Spawn effects now have durable intent/possible-dispatch claims. Attachment and
acknowledgement commit atomically under run/attempt/prior-agent fences. A live
retained-agent retry acknowledges prompt delivery; an exited-agent retry may
replace only the expected prior identity. Interrupted unattached dispatch parks
Unverified; deliberate retry requires a separate not-performed attestation.


### Human wait and retirement verification update

The generic Working/permission regression failed before the typed wait change:
one test failed because it emitted completion effects while permission remained
unanswered. The subsequent focused Rust run passed 494 tests; the final small
wait-projection changes require the next run. Human waits retain kind, request
identity when available, provenance, timestamps and response time. Permission,
question, input and review approval are distinct. A response cannot supply
lifecycle completion. Missing correlation is retained and displayed, not guessed.
Claude/Codex request and response callbacks now normalize exact request IDs. Fixture tests cover permission separately from question/approval, both request/projection arrival orders, and stale projections after responses. Live request/response delivery remains unverified.

Legacy retirement now journals the original session incarnation, blocks replacement
spawns and Circuit borrowing during cleanup, and tears down only the observed
process generation. A second independent review found no further concrete ownership
regression. This is deterministic/source evidence; live cutover is still pending.


The next full Rust run completed with 3,782 passed, 4 failed and 24 ignored.
The failures were three legacy recovery fixtures missing the new retirement table
and one command assertion expecting activation instead of the retirement error;
those fixtures/assertions were corrected and await the next full run. A subsequent
focused native-wait run passed 496 tests. The completion callback audit then found
that a legacy success callback could bypass ownership evidence; the core now
accepts it only for empty-prompt allocation readiness. Positive routing tests use
explicit synthetic terminal/ownership observations instead.

The latest full Vitest run reported 3,539 passed, 2 failed and 1 skipped. One
failure is the radius audit already reproduced at the recorded baseline. The
other was an execution-context navigation failure in the mock-browser node-header
check; its standalone rerun passed all 4 tests. This rerun does not turn the
original full command into a pass. Source lint and the documentation gate passed;
TypeScript passed after Rust regenerated the cancelled legacy state binding.


### Latest Windows verification: retirement, history and reconciliation

The live retirement smoke exposed an Idle transition that caused the frontend to
try to restart retained legacy work. Cleanup now commits Suspended and its durable
acknowledgement together, and preserves Archived nodes. The archived-node test
failed before the fix and passed afterward. A rebuilt dev app passed the startup,
UI-open and UI-reload checks for fixture Mesh 44 / node 122: no new session,
retained cancelled history, no conversion, legacy concurrency 7 unchanged while
Circuit capacity changed from 3 to 4. The 240 CSS-pixel panel was inspected.

The focused Rust suite subsequently passed 507 tests. Additions cover Blueprint
availability without a prior run, deletion protection, pinned configuration and
wait history, run/step capacity waits, continuation effect phases, partial
foreground conflict reconciliation, and transcript advancement between observation
and commit. The Blueprint deletion test failed against the earlier implementation.
The focused Blueprint/history UI run passed 63 tests. TypeScript and source ESLint
passed before the final transcript guard change; final broad checks remain pending.

Codex 0.156.1 / gpt-6-luna on native Windows produced the controlled final reply in
the real dev app. Its exact-session rollout established foreground termination;
Buildmesh retained Unverified because owned-work coverage is unavailable. Hook
execution still reports exit code 1 in the TUI. Separate Codex exec probes returned
the expected reply but delivered no callbacks, including a direct capture hook;
these are not successful hook-delivery evidence. The real app's generated Windows
callback succeeds when invoked directly against the live fixture, narrowing the
remaining investigation to the harness execution boundary. Claude was not retried.

Current live fixtures and screenshots are temporary verification artifacts.


### Rebuilt Windows verification: 2026-09-25

The acceptance snapshot was rebuilt with `scripts\run-dev.ps1 -CdpPort 9223` into the
isolated `release-dev` target. No stable Buildmesh process was stopped. Real
WebView2 and Tauri IPC verification used Codex CLI 0.156.1, `gpt-6-luna`,
Windows PowerShell, and a synthetic no-tools prompt on Mesh 44 / Circuit 45.

- Run 45 / Agent Node 126 recorded one Codex task start, one task completion,
  one smoke prompt, and no OSC 10/11 user input. The history contains the exact
  session's authoritative foreground termination and separately records that
  Codex rollout cannot establish owned child/background work. The UI showed
  that limitation as Unverified with Recheck available.
- Real IPC Recheck kept attempt one, returned to Unverified, and did not add a
  prompt or OSC input. Cancellation then left the run and step terminal with
  attempt one and 24 history entries. Run 44 from the first attempt was also
  cancelled after it exposed a missing-current-evidence recheck path; the
  adapter now returns an explicit unresolved event when an explicit Codex
  recheck cannot obtain current input/session evidence. Its regression passes.
- The rebuilt app provisioned the Windows Codex callback with quoted
  `--data-binary "@-"`. The callback was exercised through PowerShell and cmd
  against a local HTTP receiver in the Rust regression. Run 45 recorded zero
  native hook receipts. Codex's native receipt adapter deliberately retains
  only request-correlated human waits, so this no-tools run could not establish
  whether ordinary lifecycle callbacks were delivered. The TUI rollout pull
  supplied the foreground evidence.
- The current-source legacy retirement smoke confirmed retained cancelled
  history and Suspended node 122 across UI reload, no automatic Circuit
  conversion or legacy restart, the read-only retained-settings view, and
  independent capacity at 240 CSS pixels. The default Review Blueprint was
  present; it is excluded from the count of user Circuits. Mesh 44's temporary
  capacity change was restored to its fixture baseline afterward.
- The OpenPr action now stores owner/repository/head before lookup or create.
  Its explicit recheck performs only an exact saved-target lookup, never
  creates a PR, and can reconcile a match after either an unknown or
  `NotPerformed` attestation without erasing the attestation history. Rust
  service/database tests cover the lookup and transition. A late matching
  result after cancellation is rejected at the durable commit fence; the run
  stays cancelled and the effect stays uncertain. The successful stepper
  result commits PR context, run/step completion, and effect reconciliation
  together. The GitHub lookup and worker result are still tested at separate
  seams; no live GitHub request or mutation was made.

Run 46 repeated the synthetic no-tools prompt in the rebuilt Windows app with
the project `hooks = true` setting, Codex's hook-trust bypass launch flag, and
an additional fixture-only diagnostic hook. The exact rollout recorded one
task start and one completion on `gpt-6-luna`. The local receiver captured
`SessionStart`, `UserPromptSubmit`, and `Stop` with the same Codex session ID.
The Buildmesh log independently recorded lifecycle-neutral session start,
turn resume, and clean turn completion for Agent Node 127. Run 46 remained
Unverified for unavailable owned-work coverage and was cancelled at attempt
one; the diagnostic hook was removed afterward. The exact event/session/log
and durable cancellation record is `.tmp/evidence-1889-live-hooks.json`.
This verifies live callback transport and Buildmesh's ordinary lifecycle
handling on Windows Codex 0.156.1. The zero native receipt count is expected
for this no-tools prompt and does not test a human wait.

Run 45's observation evidence predates the commit-fence follow-up below. Run 46
verified hook delivery, not the newer stale-completion commit path. Owned
child/background work, live human question/permission round trips, process
restart and reattachment, and Claude, WSL, Linux, and macOS remain unverified.
No full #1889 acceptance claim is made.

The follow-up closes the stale-completion wedge found in standards review. If a
newer prompt or session change rejects a Codex completion at commit, the worker
restores the pre-observation snapshot and commits that same attempt as Unverified
through the normal run/step fence. The rejected report and lifecycle facts are
not persisted. The regression exercises the observation/persistence boundary;
the newly added path has not been repeated in the live app.

#### Latest automated checks

| Check | Result | Scope / limitation |
|---|---|---|
| Full Rust library suite | 3,813 passed, 24 ignored | Serial current-source run; 3,837 discovered |
| Focused recheck/freshness regressions | Passed | 13 recheck tests, the Codex stale-completion boundary regression, freshness-error classification, and transcript-change rejection |
| OpenPr recheck and cancellation race | Passed | Saved-target lookup tests and stepper/database commits; 4 focused lookup and completion tests plus the cancelled-result fence test. The GitHub lookup to worker result handoff remains untested as one live path |
| Full Vitest | 3,542 passed, 1 failed, 1 skipped | Sole failure is the radius audit at `AppSettings/UsageRender.tsx:308`, reproduced at the recorded base |
| Focused history UI | 9 passed | OpenPr safe recheck action and history presentation |
| TypeScript / source ESLint | Passed | UI source build completed TypeScript and desktop/mobile Vite builds; lint reported no warnings. Rust commit-fence follow-up was covered by the full Rust suite, not a rebuilt live app |
| Clippy | Passed, exit 0 | Two existing warnings remain in `environment.rs` and `harness_catalog.rs`; neither file was changed |
| Documentation gates | Passed | `npm run test:docs`: 19 passed; `npm run check:docs`: 119 Markdown files |
| Agent diff gate | Passed | `npm run check:agent -- --base d8e3a1a780599cbdcece4710df2b603860065e7a` covered the committed diff before push |
