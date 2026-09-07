# Circuit reliability and recovery audit

Base: `72e34d82981f927252900d39c2b4b05666bb36be`. Stable ledger read through
SQLite read-only mode on 2026-09-07; last inventory at 14:18 UTC. GitHub PRs,
comments and workflow state were inspected with authenticated read-only `gh`
requests. The stable app and its agent sessions were not modified.

Implementation tracked in [#1644](https://github.com/alondero/buildmesh/issues/1644).

## Findings

The 78 retained runs comprised 8 completed, 50 failed, 18 pending, 1 running
and 1 cancelled. These span older implementations and are not a failure rate
for this patch. Run 53 (issue #1531, PR #1643) remained active at the snapshot.

1. **Review completion was treated as approval.** The issue preset used an
   ordinary turn classifier for its reviewer. A completed critical review
   therefore routed to feedback, regardless of its verdict. There was no
   approved exit: three rounds always exhausted the retry gate. The exhaustion
   notification then let the entire graph become `completed`. Runs 48, 50 and
   52 demonstrate this with critical reports still in `node.reviewer.output`.
   Borrowed-source review run 76 also exhausted with a changes-requested report
   but was recorded as completed.
2. **Terminal agents retain pool capacity.** The optional global pool was 6.
   Three retained implementation agents (3653, 3660, 3672) plus active run 53's
   two-slot lease left one slot, insufficient for another two-agent circuit.
   The pending queue could therefore stall below the mesh run limit. Increasing
   the mesh limit would not solve that constraint.
3. **Timeouts were not all failed launches.** Runs 44 and 46 had reports asking
   questions while planning; run 47's finish report explicitly said no production
   implementation existed and plan-mode approval was still needed. Run 45's
   evaluated output reported a connection lost mid-response. Run 79 exhausted
   its fresh-report wait on a Grok source. These need different recovery actions.
   Missing report evidence does not establish that the process never spawned.
4. **Automatic failure cleanup destroyed recovery anchors.** It called agent
   deletion with worktree removal enabled. That deletes the node/identity and
   queues disk reclamation. A deadline must release execution capacity without
   discarding unfinished implementation work.
5. **CI evidence was absent.** GitHub reported the Build workflow as
   `disabled_manually`; all 12 open PRs returned empty check rollups and no formal
   reviews. A clean merge result from GitHub means no textual conflict, not that
   the implementation is correct. Review comments contain important evidence
   that GitHub's formal review decision does not capture.

## Open PR inventory

This is recovery triage, not a fresh code review of every PR. No PR below was
established as ready to merge. Head prefixes identify the inspected revisions;
later pushes require another check.

| PR | Inspected head | Circuit / next action |
| --- | --- | --- |
| [1643](https://github.com/alondero/buildmesh/pull/1643) | `955cdb42` | Run 53 active; latest review still critical. Let the implementation finish, then obtain a fresh verdict. |
| [1640](https://github.com/alondero/buildmesh/pull/1640) | `a0aea80e` | Run 52 falsely completed. Latest critical review names an earlier commit; review the new head before merging. |
| [1639](https://github.com/alondero/buildmesh/pull/1639) | `d837dfa4` | Run 51 timed out during feedback. Resume from the PR branch and address/reassess the latest findings. |
| [1638](https://github.com/alondero/buildmesh/pull/1638) | `5793f317` | Run 50 falsely completed. Author reports fixes after critical review; independent approval of this head is missing. |
| [1637](https://github.com/alondero/buildmesh/pull/1637) | `4018b3e6` | Run 48 falsely completed; GitHub reports conflicts. Resolve conflicts, verify, and review the resulting head. |
| [1635](https://github.com/alondero/buildmesh/pull/1635) | `b148fdae` | Run 43 exhausted wrap-up retries; last reviewer comment is critical. Recover branch, finish verification and get a new review. |
| [1634](https://github.com/alondero/buildmesh/pull/1634) | `8c643c94` | Run 42 review gate waited for input. Latest comment requests changes. |
| [1633](https://github.com/alondero/buildmesh/pull/1633) | `5efe58d3` | Run 41 review gate timed out after a work-remaining verdict. Latest comment remains critical. |
| [1630](https://github.com/alondero/buildmesh/pull/1630) | `641b2a88` | Run 37 review gate timed out; runbook review still requests changes. |
| [1627](https://github.com/alondero/buildmesh/pull/1627) | `b35b7a46` | Run 33 review gate timed out. Latest review criticizes the rewritten implementation. |
| [1616](https://github.com/alondero/buildmesh/pull/1616) | `f812657d` | Run 29 failed wrap-up/cleanup. Latest review requests substantial corrections. |
| [1614](https://github.com/alondero/buildmesh/pull/1614) | `ffbfe4b5` | Separate circuit UI/timeout work; no review or check evidence in the inspected PR. |

## Recovery procedure

- **Still-running run:** inspect its current step and agent before starting a
  second session. Questions or permission requests require the actual answer;
  a paused run can use Resume. Do not treat missing approval as granted.
- **Failed run with a saved implementation:** resume that session from Archive
  (or continue its existing node), inspect the worktree and latest review report,
  then finish the remaining authorized work. A terminal run's Resume button is
  not a retry mechanism; request a new review after the repair.
- **Older run whose node/worktree was removed:** recover the pushed PR branch
  from the PR Probe. Read its review comments before changing code. Unpushed
  changes deleted by earlier cleanup cannot be promised recoverable.
- **No PR yet:** runs 38/44/46/47 need their actual planning blocker resolved;
  run 45 needs provider connectivity restored. Check Archive and local branches
  first, then restart the issue only if no recoverable implementation exists.
- **Queue blocked by retained finished nodes:** once their work is preserved and
  no manual turn is running, stop/archive those nodes to free the optional pool.
  Historic retained sessions are not automatically opted into new cleanup.

Suggested recovery prompt for an existing PR session:

> Continue the existing PR branch. Read the issue and every unresolved review
> finding, inspect the current code, and fix each valid finding with appropriate
> regression evidence. Explain any rejected finding with concrete evidence.
> Preserve existing work, run the required checks, commit and update this PR.
> Do not declare it approved: request an independent review of the final head.

## Implemented changes

- The issue preset uses the existing semantic ReviewVerdict gate. Explicit
  approval and changes requested route separately; ordinary review completion
  is insufficient. Exhausted and blocked review runs end failed, including the
  borrowed-source preset. Ordinary nonreview fallback graphs retain their policy.
- Saved default issue graphs migrate once, preserving spawn overrides and custom
  prompts. Customized wiring/order is left intact. An old ordinary classifier's
  completed result cannot authorize a newly introduced approval branch.
- Automatic terminal cleanup stops and archives owned agents. Session identity,
  worktree and ledger association survive. An atomic per-agent cleanup receipt
  and cleanup generation claim prevent a stale sweep from archiving a later
  resume; spawn checks the claim before launching. Kill/archive failures retain
  the claim and emit no completion event.
  The frontend refresh follows the committed archive. Successful review circuits
  opt into the same process retirement so they stop accumulating pool occupancy.
- Historical completed reviews without explicit approval remain visible in
  Activity, with attention and recovery guidance. The display does not rewrite
  historical ledger rows or claim current-head merge readiness.
- Blocked review verdicts fail immediately and deliver the recovery notification;
  failed terminal transitions now permit only synchronous terminal effects.
- Review diagnostics use persisted preset metadata rather than node-name guesses.
  All terminal runs of the persisted review preset are retained; user-authored
  circuit history remains bounded.
- Default issue prompts describe unattended implementation and reserve questions
  for real blockers. Classification explicitly excludes a completed plan or API
  error from implementation completion. Review prompts distinguish actionable
  blockers from optional preferences and ask for the reviewed commit.

## Verification and operational limits

The initial changes-requested regression failed before the routing fix; the
replacement retry test drives three real rounds rather than manually fabricating
an exhausted gate. Standards and specification reviewers found no remaining
actionable findings after the cleanup receipt and nonreview fallback corrections.

- `scripts/check.ps1 all-ts`: passed, 2,880 unit tests (1 skipped), 62 integration
  tests, 6 agent-infrastructure tests, desktop/mobile compilation and bundle budget.
  An obsolete sidebar test fixture was repaired after reproducing its missing
  `onActivateNode` callback on the recorded base. Earlier loaded runs failed on
  timing; the complete unloaded rerun passed without weakening assertions.
- `npm run check:agent -- --base 72e34d82981f927252900d39c2b4b05666bb36be`:
  passed. No generated binding changes.
- `scripts/check.ps1 rust -SerialRust`: passed, 2,918 library tests and 18
  integration tests; 14 library tests and 1 doctest ignored. Prior failures from
  obsolete preset assertions were corrected before this full rerun.
- Real WebView2/Tauri UI: passed Activity/attention, recovery guidance, disclosure,
  keyboard minimum-width resize and no horizontal overflow assertions at 240px.
  Inspected before/after screenshots are under
  `docs/pr-screenshots/zippy-dimpled-mast/`. Fixtures and the verification runtime
  were removed after capture; the temporary baseline checkout was also removed.
- Full runtime log scan: **failed**, despite the scoped UI assertions passing.
  Both baseline and changed dev-profile launches emitted inactive-terminal resize
  errors and a null `session_started_at` identity-recovery warning for an existing
  dev node. Tracked in [#1645](https://github.com/alondero/buildmesh/issues/1645).
  No new panic content. The global shortcut registration warning occurs while
  stable and dev coexist. The screenshot helper initially used a nonrepository
  temporary mesh; its path/pool settings were corrected before the final capture.
- Strict `cargo clippy --locked --manifest-path src-tauri/Cargo.toml -- -D warnings`:
  failed on 14 diagnostics, reproduced with the same invocation in an isolated
  checkout of the recorded base. No new diagnostics; existing debt is tracked in
  [#1491](https://github.com/alondero/buildmesh/issues/1491).

These changes require a rebuilt stable application and restart. Verification
uses the separate dev profile. They do not retroactively approve PRs, repair
unreviewed branch code, restore deleted unpushed work, answer permission prompts,
or make a provider outage disappear. Classifier/verification observations still
run synchronously within their existing bounded worker operations. The historical
mixed wrap-up/review ledger was suspicious, but the exercised current three-round
path did not reproduce stale scheduling; no speculative scheduler rewrite was made.
Durable infrastructure retry budgets and reconciled reopening of exhausted runs
remain tracked in [#1592](https://github.com/alondero/buildmesh/issues/1592).
