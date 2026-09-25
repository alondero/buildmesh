# Circuit reliability acceptance

Implementation evidence for [#1889](https://github.com/alondero/buildmesh/issues/1889), governed by [#1850](https://github.com/alondero/buildmesh/issues/1850). This is a working acceptance record, not a claim that the specification is implemented. Baseline: `d8e3a1a780599cbdcece4710df2b603860065e7a`.

Source inspection, deterministic automated checks, and live delivery are separate evidence tiers. No aggregate reliability percentage is asserted. Unless recorded otherwise below, platform evidence is Windows, development configuration; harness versions and live delivery remain unverified.

| Scenario / stories | Contract and invariant | Owning seam / automated layer | Evidence and result | Remaining gap |
| --- | --- | --- | --- | --- |
| Lost/delayed hooks, missing optional tokens, stale/late observations (1-8, 14) | #1845: identity fences, stable deduplication, bounded reconciliation | `observation`, native receipts, Codex observer; pure and private DB tests | Windows deterministic tests pass; native request receipt/commit/reopen and delayed start/stop regressions exercised. Current Codex 0.156.1 / Luna live pull recorded below | This live Codex run received no native hook receipts. Claude production submission correlation is unavailable; other harness strategies remain explicitly unsupported |
| Child/background work (9, 11-13) | #1844: foreground termination and all owned work must be terminal | `WorkEvidence`, atomic observation batches; pure tests | Windows scripted ownership tests pass, including late child termination and missing registry entries | No available live harness establishes complete owned-work coverage. Codex live result stays Unverified |
| Human waits (10, 15, 21) | #1846: human waits do not expire or grant authorization | Typed wait observations, stepper, history UI | Windows regression reproduced generic Working incorrectly clearing permission; typed identity/request matching and UI tests added | Claude/Codex exact-request callbacks are wired; uncorrelated waits remain open with an explicit limitation. Live response checks pending |
| Restart and cancellation (22-23) | #1846: terminal cancellation and identity-proven reattachment | Ledger, worker restart, process registry; serial Rust tests | Windows tests pass for cancelled-run fences, ambiguous spawn recovery, receipt reopen, and process generations. Current rebuilt app cancellation left Codex run 45 terminal at attempt one with history retained | Current app process restart and identity-proven Codex reattachment remain untested |
| Unknown effects and recovery races (16-21) | #1846: uncertainty never authorizes replay | Stepper, transactional journal, worker dispatch; serial Rust tests | GitHub, prompt and spawn claims survive reopen. Attachment acknowledgement is atomic; injected history failure rolls it back. Competing operator revisions are fenced. OpenPr recheck uses only the saved owner/repo/head lookup; deterministic tests include `NotPerformed` then a matching PR result | No live GitHub lookup was made. Other effect kinds and continuation still need a complete typed journal audit |
| Operator history and outcomes (24-28) | #1847: append-only causal trace and actionable uncertainty | Ledger, IPC, rendered Probe; Rust and Vitest | Typed observation/classification provenance, effect history, scrubbed complete/partial/unavailable reports, operator reasons and capabilities exercised. Current rebuilt WebView2 shows Codex's ownership limitation and same-attempt Recheck | Wait/capacity/configuration history completeness remains under audit; current live viewport was not the 240px Probe check |
| Review snapshots/continuation (29-32) | #1848: frozen graph/configuration and successor deduplication | Review ledger, launch capture, blueprint UI; Rust/Vitest/live IPC | Frozen effective launch configuration and graph tests pass. Live blueprint copy was disabled/manual/independent; original mutation rejected | Current build continuation live check, blueprint availability/deletion audit pending |
| Legacy retirement/capacity (33-36) | #1849: retained history, no conversion/restart, separate capacity | Startup retirement, spawn/borrow claims, generation-fenced teardown, retained-settings UI | Windows deterministic cutover/reopen tests pass; no Circuit conversion or capacity transfer. Current source build passed real dev IPC, retained-node, 240px, and reload checks | Startup cutover was covered; a forced process crash during cleanup and cleanup retry remain untested |
| Classification (38-40) | #1850: exact fresh report interpretation cannot prove lifecycle | Immutable report envelope and pure classifier gate | Windows regression suites pass for wrong session/attempt/report/input, delayed children and invalidated evidence. Complete report retained separately from interpretation | All-harness report coverage remains incomplete. Optional synthetic fast-model comparison deferred |


Every supported observation strategy needs recorded capabilities, source, freshness bounds, child/background coverage, degraded cases, harness version, platform and launch/trust configuration. Fixture parsing alone cannot establish live delivery. Unsupported or untested combinations remain unverified.

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
  against a local HTTP receiver in the Rust regression, but this live Codex run
  recorded **zero native hook receipts**. Live hook delivery therefore remains
  unverified; the TUI rollout pull supplied the foreground evidence.
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
  service/database tests cover the lookup and transition; no live GitHub
  request or mutation was made.

This live evidence predates the commit-fence follow-up below and is limited to
Windows and Codex's foreground observation path.
Owned child/background work, live native-hook delivery, human question/permission
round trips, process restart and reattachment, and Claude, WSL, Linux, and macOS
remain unverified. No full #1889 acceptance claim is made.

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
| Full Vitest | 3,542 passed, 1 failed, 1 skipped | Sole failure is the radius audit at `AppSettings/UsageRender.tsx:308`, reproduced at the recorded base |
| Focused history UI | 9 passed | OpenPr safe recheck action and history presentation |
| TypeScript / source ESLint | Passed | UI source build completed TypeScript and desktop/mobile Vite builds; lint reported no warnings. Rust commit-fence follow-up was covered by the full Rust suite, not a rebuilt live app |
| Clippy | Passed, exit 0 | Two existing warnings remain in `environment.rs` and `harness_catalog.rs`; neither file was changed |
| Documentation gates | Passed | `npm run test:docs`: 19 passed; `npm run check:docs`: 119 Markdown files |
| Agent diff gate | Passed | `npm run check:agent -- --base d8e3a1a780599cbdcece4710df2b603860065e7a` covered the committed diff before push |
