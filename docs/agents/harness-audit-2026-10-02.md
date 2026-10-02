# Agent harness audit, 2026-10-02

Contributor/maintainer evidence, baseline `7932f3f4`. This audit reviewed
repository instructions, skills, hooks, check commands, CI and tests, plus
product harness adapters, capability generation, launch wrappers, transcript
observation, checkpoints, Circuit persistence/recovery and review contracts.
The resulting repository harness is documented in
[Development harness](development-harness.md).

## Evidence and limits

A purposeful sample of 19 recent PRs covered #1967, #1969, #1975–#1978, #1980,
#1981, #1983–#1985, #1987–#1989, #1991, #1993, #1994, #1996 and #1998. All formal
review arrays returned empty; no reviewer failure-rate statistic can be inferred.
Two author comments explicitly acknowledge review findings. Other evidence is
attributed to remediation descriptions and the
[earlier infrastructure audit](infrastructure-audit.md), whose September
review-churn sample covered 40 PRs. Commit counts are not review-round counts.
The referenced private Sports Notion page was inaccessible; only the supplied
seven-component guidance was available.

| Recurring friction | Evidence | Control response |
|---|---|---|
| Tests and docs validate the same wrong premise | [#1984 author response](https://github.com/alondero/buildmesh/pull/1984#issuecomment-5927277575): harmful Retry behavior was defended by tests and primer | Observable criteria and evidence, separate independent review; green alone is insufficient |
| Async results escape mesh ownership | [#1976](https://github.com/alondero/buildmesh/pull/1976): stale strategy saves and duplicated hydration | Named mesh-isolation case; controlled ordering at the production transition |
| Native fixtures repeat imagined formats | [#1994](https://github.com/alondero/buildmesh/pull/1994): actual OpenCode SQLite records differed from export fixtures; [#1998](https://github.com/alondero/buildmesh/pull/1998): Codex context records | Preserve sanitized real native fixtures and version provenance at owning adapters |
| Guards certify incomplete paths | [#1988 author response](https://github.com/alondero/buildmesh/pull/1988#issuecomment-5931179997): shell syntax gaps and premature CI abandonment | Allowed/denied cases through real entrypoints; freshness/complete-gate checks |
| Scope skips important contracts | [#1991](https://github.com/alondero/buildmesh/pull/1991): generated bindings could skip Rust | Conservative scope, committed plus dirty/untracked inputs, binding drift |
| Environment interrupts tests before execution | [#1993](https://github.com/alondero/buildmesh/pull/1993), [#1994](https://github.com/alondero/buildmesh/pull/1994), [#1998](https://github.com/alondero/buildmesh/pull/1998): conflicting color variables and PowerShell stderr handling | Direct process execution, normalized child environment, missing-tool preflight, executed counts |
| Deadlines/retries fail to bound work | [#1980](https://github.com/alondero/buildmesh/pull/1980), [#1991](https://github.com/alondero/buildmesh/pull/1991): escaped pipes and SIGKILL race; [#1987](https://github.com/alondero/buildmesh/pull/1987): classifier exhaustion | Reuse process-tree guard, durable per-gate outcomes/logs, separate timeout |
| Readiness, completion, delivery and approval get conflated | [#1975](https://github.com/alondero/buildmesh/pull/1975), [#1981](https://github.com/alondero/buildmesh/pull/1981), [#1996](https://github.com/alondero/buildmesh/pull/1996) | Exact-tree check receipts remain separate from handoff and review verdicts |

## Seven-component architecture assessment

| Component | Existing foundation | Added control / remaining limit |
|---|---|---|
| Context | Shared CLAUDE/AGENTS entrypoint, domain glossary, engineering contract, primer and ADRs | Bounded session context and explicit task resumption; relevant module reads |
| Workflow | Verify tiers; issue implementation/review/feedback circuits | Start/update/verify/finish CLI; recorded criteria and semantic review |
| State/continuity | Durable Circuit context, steps, attempts, snapshots, effect journal and history | Worktree-local task and current-tree receipts; committed goal-linked Git refs |
| Tools/environment | Windows checks, dev launchers, strict Vitest, smoke browser, bounded CI runner/shards | Portable canonical verification plan, explicit counts and prerequisite failures |
| Guardrails | Edit/staging/documentation guards, shared diff rules, lint fixtures, generated types | Direct state-edit guard and fast lint/types; shell writes remain outside hook protection |
| Verification/stopping | Identity/freshness-bound report and review contracts | Current receipts required by finish; blocked diagnostic handoff stays incomplete |
| Feedback/improvement | Extensive incident records and regressions; durable product history | Named behavioral corpus, append-only gate/attempt/timing/recovery metrics; no measured token savings yet |

Existing product strengths should be retained: adapter-owned SpawnRecipe and
WindowsShell decisions; generated harness capability catalog; native transcript
readers; pure Circuit stepper with commit-before-effect persistence; effect
claim/replay policy; frozen run configuration; durable bounded classification
budgets; deterministic HANDOFF/REVIEW result lines. These already reduce
rediscovery and redundant model inference. Adding another generic classifier
would duplicate those controls.

## Product work still required

1. **Verify the implementation worktree.**
   [jobs.rs](../../src-tauri/src/services/circuit_worker/jobs.rs) resolves
   DeterministicVerification at `mesh.path`, while implementation agents can
   own distinct worktrees. Before integrating repository receipts into product
   completion, resolve and bind the actual source worktree and revision.
2. **Replace boolean verification with durable typed evidence.**
   [circuit_worker/mod.rs](../../src-tauri/src/services/circuit_worker/mod.rs)
   discards output and returns false for ordinary failure, missing tools and a
   fixed 120-second timeout. Extend the existing ledger/stepper with typed
   outcomes, commands, deadlines and retained diagnostics. Preserve saved graph
   compatibility and frozen-run semantics; environment outages must not spend
   implementation repair rounds.
3. **Gate review dispatch with current deterministic evidence.** Built-in
   issue/local review flows currently use report readiness and semantic review
   without a deterministic check node. Repository hooks cannot enforce every
   provider's product completion. Teach the existing finish/reviewer flow to
   consume appropriate worktree-bound evidence before publication/review.
4. **Keep finish scope bounded.**
   [finish.rs](../../src-tauri/src/circuit/finish.rs) requests full suites and
   repairable baseline fixes. Align future product prompts with selected checks
   and avoid unrelated repairs. Store reviewer findings with source revision so
   subsequent reviewers assess fixes without rediscovering the entire history.
5. **Expand live behavioral evaluation.** The seven initial cases reuse real
   test selectors but state their mock/pure-module boundaries. Add controlled
   real PTY retention, two-mesh command isolation, restart reconnection, WSL
   execution, concurrent layout/process ownership and actual lost-agent UI
   checks. Capture native format fixtures rather than invented envelopes.

The suggested existing Git-ref checkpoint premise was historical:
[migrations.rs](../../src-tauri/src/db/migrations.rs) drops the old product
checkpoint table at schema v12. Product evidence checkpoints are not restorable
code snapshots. The new developer checkpoint command records committed Git
refs under task identity; it does not add product rollback UI or automatic
destructive recovery.

## Evaluation policy

Use the harness's fixed negative cases to detect stale/partial receipts,
cross-worktree reuse, unavailable commands, timeout, zero executed tests and
hook regressions. Product cases test promises rather than coverage percentages.
Record actual platforms and evidence boundaries. Compare repeated tasks at a
fixed baseline/model/environment before claiming improvement; gate time and
attempt counts alone do not demonstrate lower token usage or causal first-pass
quality. Keep recurring lessons in production regressions and owning docs,
with short navigation from the always-on instructions.
