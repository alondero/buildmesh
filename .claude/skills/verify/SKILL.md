---
name: verify
description: Verify a Buildmesh change with scoped build, test, and runtime evidence; report failures and coverage gaps accurately.
---

# Verify a Buildmesh change

Read `docs/agents/engineering.md` for the shared verification contract. Establish the change's base commit and acceptance outcomes before choosing checks. A user request to verify does not authorize unrelated repairs or publishing.

Use `npm run verify` with the active task, or `npm run verify -- --base <commit>`
without one, for the canonical scope-selected gate and durable receipt. Read
`docs/agents/development-harness.md` for PASS/FAIL/BLOCKED/TIMEOUT, continuity,
current acceptance/review evidence and `finish`. Reuse unchanged passing gates;
do not replace a failed full check with a successful subset.

During independent-review repairs, follow
[Review and repair](../../../docs/agents/development-harness.md#review-and-repair):
batch fixes, run fast checks plus affected behavioral regressions, and expand
checks when the repair changes shared contracts or runtime boundaries. A fresh
full verify is not a prerequisite for each follow-up review. Run scope-complete
`npm run verify` for the stable candidate before handoff, then record acceptance
evidence and independent approval of that same final tree.

## Tiers

Default to standard; narrow to affected layers for documentation-only or frontend-only changes and state the scope.

- **quick:** Run `npm run test:agent`, `npm run check:agent -- --base <base>`, and focused behavior tests. For frontend changes run `npm run build`; for Rust changes compile the affected target with mobile assets built. A compile-only result is not Rust test coverage.
- **standard:** Run `npm run verify` for the scope-appropriate gates. It includes all-targets Clippy with zero warnings in touched files, locked Rust tests (CI shards run as concurrent processes), binding drift, and frontend browser smoke where applicable. The repository has an acknowledged Clippy warning backlog; do not claim a global `-D warnings` baseline is green. Inspect generated binding changes when wire types change. The existing Windows `check.ps1` targets remain focused iteration commands, with the exclusions documented in the engineering contract.
- **full:** Standard plus actual dev-profile runtime verification using `scripts\run-dev.ps1` on Windows or `scripts/run-dev.sh` elsewhere. These scripts build and launch; a separate Tauri build first is redundant. Read `../verify-ui/SKILL.md` for visible UI changes (for Probe tabs, verify layout and keyboard navigation at the 240px minimum width constraint). Capture startup log offsets and inspect only new lines as below.
- **--escalate:** Run quick, standard, full, reusing successful checks for the same unchanged tree. Stop escalation at a failure.

Playwright's top-level webServer starts Vite on 1420, not Tauri. The verify-smoke project injects mock IPC. The chromium project's specs have additional real-runtime requirements: inspect them before use. Do not stop the stable hub or arbitrary Node processes to free ports. Only stop processes owned by this verification run; report an occupied resource when ownership is unknown.

## When a check fails

Capture the actual command, exit result, first actionable error and failing test name. Diagnose and fix failures caused by the requested change; rerun the failed check and affected checks after a fix. Avoid restarting the entire expensive tier after every focused edit. Cap repeated unsuccessful repair attempts at five and report the remaining blocker.

An unrelated failure remains a failed check. Reproduce on the recorded base under equivalent conditions before calling it pre-existing. Do not use a clean diff as proof of baseline behavior, substitute cargo check for cargo test, accept paper-tiger tests that skip assertions, claim commit fixes absent from the diff, suppress runtime errors, or mark screenshots that have not been captured as complete.

## Runtime evidence

The dev launcher prints pre-launch line counts for `buildmesh.log`, `panic.log`, and `panic_early.log`. Inspect the deltas in the dev-profile log directory:

- Any new panic-file content fails runtime verification; include the message, originating file, and useful stack frames.
- New ` ERROR `, `panicked at`, `Illegal invocation`, missing-command, or spawn-failure messages fail the relevant check. Report unexplained warnings.
- A clean log and live process prove startup only. Assert the requested behavior in the real UI or at the relevant boundary.
- For a blank terminal, consult the diagnostic probes in `../use/SKILL.md`: distinguish process alive, PTY bytes delivered, and xterm rendering. Bound network reads with a real cancellation timeout.

## Report

State the base/working tree, requested scope, commands actually run, results and executed counts. Separate passed, failed, and not run. Label browser evidence as mock IPC or real backend. Include the observed acceptance outcome and any limitation that matters to the reviewer. Add a durable lesson to the owning documentation only when it changes a future decision; do not accumulate one-off recipes in this skill.

When the verification includes remote state — a pushed branch, a PR awaiting checks, merge readiness — read `Remote CI` below before reporting a verdict.

## Remote CI

Remote state comes from one run, never from a PR rollup. `gh pr checks <pr>` aggregates every workflow run the branch ever triggered, so a `pending` entry from a superseded or cancelled run keeps being reported after the live run has already finished. Resolve the run for the pushed commit — `--commit`, not `--branch`, so a new run that has not registered yet cannot hand you the previous one — then poll that run's jobs:

```powershell
$sha = git rev-parse HEAD
$run = gh run list --commit $sha --limit 1 --json databaseId --jq '.[0].databaseId'
gh run view $run --json status,conclusion,jobs
```

Inside one run there is no rollup, and a job that is still executing keeps the same job id on every poll. **A stable pending job id is what in-flight execution looks like — keep waiting on it.** Staleness only exists when comparing across runs, and the tell is that `gh run view <run>` reports `status: completed` while `gh pr checks` still lists something pending; that entry can only be left by an older or cancelled run. If `gh run list --commit` returns nothing yet, the workflow has not registered — re-poll for the run rather than reporting the push as unverified.

A red required check is not automatically a code failure — classify it before acting. List the run's job conclusions, then read the failing job's own log (`gh run view --job <job-id> --log-failed`); the `Rust tests + TS bindings` aggregate only reports that something upstream was not green, so a red aggregate with a red `Detect changes` or `Rust build` needs the upstream log, not the aggregate's. Compiler and test errors (`E0425`, a failing assertion, `test result: FAILED`) mean fix the code. Infrastructure leaves different signatures: `was not acquired by Runner of type hosted`, a lost runner with no step conclusion and no retrievable log, `apt-get` or download timeouts, or `cancelled`. Those mean re-run the failed jobs (`gh run rerun <run-id> --failed`) and change nothing — editing code in response to an infrastructure failure only adds churn on top of a healthy tree.

PowerShell evaluates a command's **output**, not its exit code, so a silent native command used as a bare condition is always false. `git merge-base --is-ancestor` prints nothing on success, which makes the condition the empty string and sends every call down the `else` branch — the check can never report success, whatever the real state:

```powershell
# WRONG - always false on success: the condition is the command's empty output
if (git merge-base --is-ancestor $sha origin/main 2>$dev) { "YES" } else { "NO" }

# RIGHT - run the command, then branch on its exit code
git merge-base --is-ancestor $sha origin/main 2>$dev
if ($LASTEXITCODE -eq 0) { "YES" } else { "NO" }
```

The same trap applies to any silent native command in a condition: `git diff --quiet`, `gh`, `cargo`. Cmdlets that return a value are unaffected — `if (Test-Path $log)` is fine — as are explicit output comparisons such as `if ((git status --porcelain).Length -gt 0)`. The `guard-antipatterns.mjs` hook and `npm run check:agent` enforce this in `src/`, `src-tauri/`, and `scripts/`; both skip Markdown, which is why the wrong form above can be quoted here to teach it.

When two commands disagree about repo state, check the commands before blaming the cache. One always-wrong command and one correct command produce two mutually inconsistent readings, and "stale refs" explains them no better than the bug that is actually there; trusting the first confident story instead of reading both commands is how a five-minute mistake becomes a ten-minute one.

Which checks are required on `main`, and what each one covers, is not restated here: read `docs/development/releasing.md#required-checks-and-branch-protection`, which is the single source of truth and stays in step with `.github/workflows/verify.yml`.
