# Development harness

Current contributor and agent workflow. The repository harness records a task,
selects checks, and refuses a successful `finish` without current check,
acceptance, and review evidence. It complements Buildmesh's product Circuits;
it does not change their state machine or automatically publish anything.

## Start and resume

Run from the intended worktree with Git, Node/npm, and installed dependencies.
Rust checks also require Cargo and the Tauri platform dependencies. Browser
smoke requires `npx playwright install chromium`. Windows worktree setup is
documented in [Worktree setup](#worktree-setup).

Create an ignored `.tmp/task.json`, for example:

```json
{
  "goal": "Preserve terminal state while switching meshes",
  "criteria": ["Switch away and back; the same terminal retains its buffer"],
  "plannedEdits": ["src/components/Terminal/TerminalRegistry.ts"],
  "nextAction": "Inspect terminal ownership and the existing retention test",
  "agent": "codex",
  "model": "record the actual model when known"
}
```

```powershell
npm run harness -- start --spec .tmp/task.json
npm run harness -- status
```

Start resolves HEAD as the immutable comparison base. Supply `base` in the spec
when resuming work that already contains commits. Unfinished tasks cannot be
overwritten. State lives in ignored `.harness/active-task.json` in this worktree,
so separate worktrees cannot share a task or receipt. `status` is the resumption
entrypoint after compaction or restart; it includes goal, criteria, plan, phase,
decisions, blockers, next action and check status. Do not load the entire
architecture primer: follow the relevant section to its owning module.

Use `update --spec .tmp/progress.json` for phase, next action, decisions,
blockers, acceptance evidence and review result. Goal, identity and base stay
fixed. Phases are understand, plan, implement, verify, review and blocked;
`finish` alone sets complete. Each criterion needs one evidence string, in
criterion order, describing the command/artifact and observed outcome. A review
entry identifies reviewer, findings, summary and an explicit
APPROVE/REQUEST_CHANGES/BLOCKED verdict. Completion requires APPROVE with no
unresolved findings.
Both evidence and review are bound to the tree when recorded; editing code
requires recording them again.

```json
{
  "phase": "review",
  "nextAction": "Finish after independent review",
  "evidence": ["terminal-container-reuse regression passed; real PTY check recorded separately"],
  "review": { "reviewer": "independent reviewer", "verdict": "APPROVE", "findings": [], "summary": "Approved current revision" },
  "blockers": []
}
```

## Review and repair

This workflow adopts batched findings (option B) and fast repair checks
(option D) from #2122. During an independent review's repair loop, use fast checks
plus focused behavioral regressions; run scope-complete verification when the
candidate is stable and before successful handoff. Final completion continues
to require current verification, acceptance evidence and independent APPROVE
for the same tree.

Use the [review skill](../../.claude/skills/review/SKILL.md) with the task's
immutable base, complete change, acceptance criteria, existing findings and
available evidence. The first pass checks the whole change against the spec
and engineering contract, reporting all substantiated findings together and
separating required corrections from optional suggestions. Exhaustive means
a complete best-effort pass; repairs can still introduce new defects.

After REQUEST_CHANGES:

1. Record the verdict and findings with `harness update`, and keep the task
   active while repairing the batch. Review updates do not require a fresh
   full verification receipt.
2. Run fast checks and the regression tests that exercise each affected
   production boundary. Claude's advisory `.harness/fast.json` covers agent
   rules, changed-file lint and TypeScript checking; it runs no behavioral or
   Rust tests. Every other harness — Codex included — runs
   `npm run harness -- check`, which maps the diff onto that same fast set plus
   `vitest run --changed <base>` and `cargo test --lib <module>::` for the
   changed files. It is advisory and writes no receipt: a scoped check that
   passes (or that executes zero tests) is not completion evidence, and
   `npm run verify` still decides completion.
3. Broaden checks during repair when changes affect shared contracts,
   migrations, security, lifecycle ownership, dependencies or platform/build
   behavior. A failed full check remains unresolved until the failing check
   and affected checks pass; a passing subset supplies diagnostic evidence.
4. Before pushing the repair, run `npm run self-review -- --base <base>`. A
   repair that adds explanation or tests adds claims the next round audits, so
   delete or soften a disputed claim rather than defending it at length.
5. Give the reviewer the repair diff, previous findings and executed checks.
   Follow-up review checks every finding's resolution, new defects and the
   repair's interactions with the whole change. A reviewer without that
   history, or a change that expands scope, needs a complete pass.

Once the reviewer has no outstanding corrections, run `npm run verify` for
all applicable gates, reusing PASS results only while their declared inputs
match. Record acceptance evidence and independent approval of the complete
final revision. A follow-up approval may reuse earlier inspection, but it
must explicitly cover the current whole change. Source edits during or after
this final verification require refreshed verification, acceptance and review
evidence before completion. Fast results never replace the final receipt.

Continue the repair loop within the active task; use progress updates for
intermediate status. `finish` and the Stop hook still reject incomplete or
stale evidence. CI and release checks remain independent safeguards; mock
browser checks do not replace required real-runtime acceptance evidence.

## Worktree setup

Use `npm ci` in a fresh checkout. In a secondary worktree, link the main
checkout's installed packages instead:

```powershell
npm run harness -- link-deps                  # packages from the main checkout
npm run harness -- link-deps --cargo-target   # also share src-tauri/target (below)
npm run harness -- link-deps --from <path>    # another checkout instead of the main one
npm run harness -- unlink-deps                # remove the links link-deps made
```

`link-deps` finds the main checkout from `git worktree list` and refuses, with
a message to run `npm ci`, when `package.json` dependency fields or
`package-lock.json` differ, when the source has no `node_modules`, or when the
worktree already has a real install. Each package is linked individually under a
private `node_modules` root; `.bin` shims are copied and bundler caches
(`.vite`, `.vite-temp`) stay local. Running it again refreshes its own links.
Do not run `npm install` or `npm ci` over linked packages: they write through
the links into the main checkout. Run `unlink-deps` first.

`unlink-deps` removes links with non-recursive unlinks, so it does not prompt
and leaves the main checkout's files in place. It also removes a manual whole-directory
`node_modules` junction and a linked `src-tauri/target`, and leaves a real install
alone. In a `node_modules` that `link-deps` created, it removes the links found on
disk (any symlink or junction there, including ones left by an interrupted
`link-deps`), so rerunning `unlink-deps` or `link-deps` recovers from an interrupted run.
Do not remove a junction with `Remove-Item`: PowerShell prompts, which hangs a
headless runner indefinitely, and `-Recurse` can delete the junction's target.
Reviews that must leave the dependency checkout's caches alone use
[isolated review checks](#isolated-review-checks), which share the same linking code.

Run Vitest with `NODE_ENV=test`; a production React runtime makes render tests
fail with `React.act is not a function`. The harness sets the environment for
its test and build gates. Build `dist/mobile/` with `npm run build:mobile` before
Cargo tests, and rebuild it after mobile source changes.

On Windows, run Rust checks through `scripts/rust-test-shards.mjs` or
`scripts\check.ps1`. A junction from `src-tauri/target` to the main checkout's
absolute Cargo target (`link-deps --cargo-target`) reuses dependencies and
satisfies the git gates (ADR 0044).
Cargo can otherwise report a different worktree's same-name test binary as
fresh; the runner checks the worktree stamp, rebuilds this repository's packages
when needed, and copies binaries out before running them. A task-local
`CARGO_TARGET_DIR` avoids that package rebuild when worktrees verify concurrently.

## Isolated review checks

Run checks against a pinned committed revision without writing build output or
cache files into the reviewed checkout:

```powershell
node scripts/review-checkout.mjs --ref <reviewed-sha> -- npm run build
node scripts/review-checkout.mjs --ref <reviewed-sha> -- node --test tests/agent-infra/check-agent-diff.test.mjs
```

The helper creates a detached Git worktree under this checkout's ignored
`.tmp/review-checkouts/`, prints the resolved revision and path, runs the command
there, and removes its checkout on normal success or failure. Source edits and
untracked files are not included; review those separately in the original diff.
For several commands in one checkout, run a task-local Node script as the command.
A killed helper may leave its printed checkout behind; inspect that path and use
`git worktree remove --force <printed-path>` to remove only that checkout.

Installed packages are linked individually, with a private `node_modules` root
and copied bin shims. Vite's `.vite` and `.vite-temp` caches stay in the review
checkout. Cargo output, npm cache and temporary files also use private paths.
Package directories themselves are shared: use this mode for build and test
commands that do not modify installed packages. Dependency inputs must match
the source checkout. Use `--no-deps` to avoid sharing packages and run a
script that installs dependencies before checking a different dependency revision.

Git metadata matters: a source extraction without `.git` changed Tailwind's
source discovery and produced a false bundle-budget finding. Use this helper
or a real detached worktree, rather than adding a synthetic `.git` directory.

## Detached dev processes

Use a background server for repeated browser checks:

```powershell
npm run dev:bg -- --port 3999 --timeout-seconds 60
node scripts/ui-shot.mjs --mock --mock-url http://127.0.0.1:3999 --out .tmp/ui-after.png --steps .tmp/steps.mjs
npm run dev:stop
```

Start binds loopback with a strict port, waits for HTTP readiness, and returns
the PID, URL and `.harness/logs/dev-server.log` path. Ownership lives in
`.harness/dev-server.json`, including the PID and a private control token.
Repeated starts reuse this worktree's server; a different requested port
requires stopping it first. Occupied ports fail rather than adopting another
server. Stop confirms ownership through the private loopback endpoint and
closes only that Vite instance. Stale dead-PID records are removed; a live PID
whose ownership cannot be confirmed is left alone with a diagnostic. Stop
retains that record on failure; a new start discards the unconfirmed record
and tries a fresh strict-port launch without stopping the old PID. An occupied
port still fails. CLI operations serialize with `.harness/dev-server.lock`
and reclaim dead owners.
Startup failure or timeout ends the newly launched child. Its handles are
redirected, so the invoking shell can exit while the ready server keeps running.

Native app launchers support `scripts\run-dev.ps1 -Detach -CdpPort 9223`
and `scripts/run-dev.sh --detach`. They still build synchronously, then wait up
to 30 seconds for a new `started|ready` log entry while the launched PID is
alive. Configure that readiness budget with `-ReadinessTimeoutSeconds <n>` or
`--readiness-timeout-seconds <n>` (1–300 seconds). A panic, early exit or timeout
fails the launch and cleans up its PID. Success prints the PID and app log path
and leaves the app running. Windows uses an independent ShellExecute launch
with normal GUI visibility; macOS/Linux redirect console output to
`.harness/logs/dev-app.*.log` with nohup.
Stop the printed dev PID when finished. A dev instance belongs to the worktree
that launched it: the launcher records it in `.tmp/dev-instance.json` and stops
only that PID, plus any `buildmesh-dev` running *this* worktree's executable.
A linked worktree also gets its own app-data profile and port range, so two
worktrees can each hold a dev instance without touching each other's ports or
agents; the stable hub is unaffected either way. See
[startup-and-profiles](../development/startup-and-profiles.md) for the identity
rules and the `BUILDMESH_PROFILE_DIR` / `BUILDMESH_PORT_OFFSET` overrides. Each
launch prints its `Dev ports:` and `Dev profile:` lines, so a check can name the
ports it expects. Without detach, the existing launch check retains its
process-alive fallback.

UI-only checks use `ui-shot --mock` first on every host, with functional
assertions, inspected screenshots and elapsed time recorded in the PR. This
needs Vite and Chromium but no native build. Use the native dev profile when
the backend is part of the change or required acceptance needs real runtime
evidence; see the [UI verification skill](../../.claude/skills/verify-ui/SKILL.md).

## Scoped check

`npm run harness -- check [--base <commit>]` is the fast check every harness can
run, so a session without Claude's PostToolUse hook still gets real coverage from
the diff instead of running focused commands by hand. It maps the changed paths to
the agent rules, changed-file ESLint and `tsc` gates the hook already runs, adds
`vitest run --changed <base>` when a frontend file changed, adds
`cargo test --lib <module>::` for each changed Rust module, and re-runs the
`#[path]` module-table guard when a Rust source file changed. The mapping is
table-driven in `scripts/harness-check.mjs` and tested per category.

It is deliberately weaker than `verify`, which stays the completion gate:

- It writes no receipt, takes no `.harness/lock`, and cannot finish a task, so it
  can run beside a verify.
- A check that executes zero tests is not behavioural evidence. Report the
  executed count; `npm run verify` supplies the required coverage.
- A change with no module filter — the crate root, a manifest, the build script,
  an integration test — runs the whole `cargo test --lib` suite, and a path under
  `src-tauri/tests/` is not covered by `--lib` at all, so it needs `verify`.
- A frontend selection is narrower than `planGates`, and deliberately so. The plan
  treats an unknown path as both product layers, and `categorisePath` calls every
  `tests/` path 'frontend'; this check selects Vitest only for the trees Vitest
  actually runs over — `src/`, `mobile/`, `public/`, `tests/unit/`,
  `tests/integration/`, `tests/setup/`, `docs/brand/` and the vite/vitest configs.
  An edit to `tests/agent-infra/**`, `tests/e2e/**` or `tests/lint-fixtures/**`
  does not start Vitest, because those tests are not in the Vitest run and
  `vitest --changed` would select nothing.
- It runs the `#[path]` guard because a Rust-only diff is exactly the one that
  skips `agent-tests`, where the guard otherwise lives. `agent-tests` and the
  `rust-paths` gate call the same `pathIncludeDrift` function, so they cannot
  disagree about the table.

## Verify and finish

Run `npm run verify` in the foreground when it fits. The frontend and Rust
gates run as two concurrent lanes (see below), so an idle machine finishes a
Rust and frontend run in roughly the time of the longer lane, inside a single
ten-minute tool call. Documentation- and harness-only changes take seconds.
When the run may not fit (a busy machine, or a harness that caps calls lower),
start it in the background and block on it with `harness wait` rather than
polling with sleeps:

```powershell
npm run verify                       # foreground; or start it in the background
npm run harness -- wait --max-seconds 540
npm run harness -- update --spec .tmp/progress.json
npm run harness -- finish
```

`wait` blocks until no harness operation holds `.harness/lock` (a verify holds it
for its whole run), then prints the receipt summary: the outcome line, then each
nonpassing gate with its reason and log path, in at most about 20 lines. It exits
0 on PASS, 1 on FAIL, 2 on BLOCKED and 124 on TIMEOUT. If the limit passes while
the run is still going it prints the gates finished so far, exits 124, and can be
called again; the run itself is not stopped. It also reports BLOCKED, never a
stale PASS, when no verify is running and the last receipt is unfinished or
predates the run that just ended. `--max-seconds` defaults to 540, under the
ten-minute call cap. `wait` is read-only and does not take the lock, so it
behaves the same on every harness. Each gate also writes
`.harness/logs/<timestamp>-<gate>.log` as it runs, so a log can be read while the
run is still going. A killed run is recovered automatically (see the lock
paragraph below).

`verify` first prints the advisory [self-review prompts](self-review.md):
formatting-only hunks inside behaviour commits, absolute claims in added
comments and documents, and a seven-line checklist digest. They compare the
working tree with the verification base, never fail a gate, and are not part of
the receipt's outcome; a plan substituted by a test skips them. When they
flagged something, the receipt records the count and `wait` adds one line with
the command that prints them again (`npm run self-review -- --base <commit>`),
which is also the cheap check to run before pushing a repair.

### Lanes and the machine-wide slot limit

Quick infrastructure gates (whitespace, shared rules, docs, README, process
spawns, contract tests, ESLint) run first, in order. The product gates then run
as two lanes at the same time, each keeping its own order and fail-fast:

The agent-infrastructure contract suite has a fifteen-minute deadline on
Windows, where its CLI subprocess and PowerShell startup costs can exceed the
five-minute deadline used elsewhere. Its assertions and failure handling are
the same on each platform.

| Lane | Order |
|---|---|
| Frontend | `frontend-build` → `bundle` → `frontend-tests` → `browser-smoke` |
| Rust | `rust-format` → `rust-clippy` → `rust-tests` → `binding-drift` |

`rust-clippy` and `rust-tests` also wait for `frontend-build` to pass: that build
empties `dist/` (including `dist/mobile/`, which the Rust crate embeds), so the
crate must not compile while it runs. `rust-format` only parses and overlaps it.
`cargo test` rewrites the ts-rs bindings in `src/types/generated/` on every run,
while the Vite dev servers behind `browser-smoke` and the `ui-shot` tests run
beside it; `vite.config.ts` therefore excludes that directory from the watcher,
since a reload would detach elements mid-test.
With no frontend scope the Rust lane opens with `mobile-build` and has nothing
to wait for. After any nonpassing gate no new gate starts in either lane, gates
already running finish so their logs and receipt rows stay complete, and a gate
whose dependency did not pass is not run. The receipt lists gates in plan order
whatever order they finished in, and its `durationMs` is wall time.

When both lanes hold a heavy gate, each of `frontend-tests` and `rust-tests` is
limited to half the cores (`VITEST_MAX_WORKERS`, `RUST_TEST_THREADS`, and two
concurrent Rust test processes). Left at one worker per core each, the two
suites oversubscribe the machine and tests with real-time bounds (process
spawns, pipe drains) fail on load instead of on a defect. A lone heavy gate
keeps the whole machine.

Gates from several worktrees compete for the same CPU, so the two heavy gates,
`rust-tests` and `frontend-tests`, take a machine-wide slot first: at most 2 run
at once across all worktrees (set `BUILDMESH_HEAVY_GATE_LIMIT` to change it). A
gate that has to wait prints `QUEUED <gate>: ...` naming the holders, repeats
the notice every minute, and records `queuedMs` in its receipt row; its own
`durationMs` and deadline start only once it holds a slot. Slots are files under
`%LOCALAPPDATA%\buildmesh\gate-slots` (`$XDG_STATE_HOME` or `~/.local/state` off
Windows; `BUILDMESH_GATE_SLOTS_DIR` overrides). A slot held by a dead process is
reclaimed, so a killed run needs no cleanup. The per-worktree `.harness/lock` is
unchanged.

Without an active task, `npm run verify -- --base <commit>` runs the same checks
and records a standalone receipt. It cannot finish a task. `--full` expands
verification to both frontend and Rust even for documentation changes.
A run resolves `--base` to a full commit once, before it publishes the identity
its gates read. `runIdentity` hashes the base string, so a short sha and its
full hash are two identities for one commit, and both sides of the
`binding-drift` comparison use the resolved one (issue #2350).

That identity also covers the environment the gates ran in — `NODE_OPTIONS`,
`RUSTFLAGS`, `CARGO_TARGET_DIR`, `CC`, `CXX` and `TS_RS_EXPORT_DIR` — so a
receipt describes one run rather than one tree. Keeping `CARGO_TARGET_DIR` in
it is a deliberate call, not an oversight: the variable redirects build output,
so the harness treats a change to it as a change of run and asks for
verification again (issue #2377). Verifying under one target directory and
finishing under another therefore reports a stale receipt, which is the
intended verdict and used to be indistinguishable from a missing check. The
receipt now records the run identity it was written under, so that verdict
names what differs (`CARGO_TARGET_DIR: recorded D:\shared\target, now unset`),
`harness status` lists the same differences under `verification.differs`, and
the repair is either to re-run `npm run verify` here or to run the harness in
the environment that verified. A stale receipt caused by a source change, and a
receipt written before this field existed, keep the plain `Run npm run verify`
advice.

Committed, staged, unstaged, deleted and untracked paths determine scope.
Unknown paths select both product layers; generated wire types always select
both. Executable hooks and harness tests still run infrastructure gates.
Adding only harness package scripts, or the exact `.harness` ESLint ignore,
keeps infrastructure scope; dependency/build/rule changes retain product gates.

The contract-test gates are scoped by their own inputs rather than run on every
change. Each declares the paths it provably does not read, so `agent-tests`,
`readme-tests`, `lint-tests` and `lint-fixtures` join the plan only when a changed
path is one they read, and a Rust- or frontend-only edit skips all four (issue
#2221). `docs-tests` reads all of `docs/**` plus every `src-tauri/src/**/*.rs`, so
it stays unconditional. A gate with no declared inputs reads the whole tree and is
never skipped; `--full` and an empty diff run the whole plan.
`tests/agent-infra/harness.test.mjs` pins each mapping, so a gate that starts
reading a path it does not declare fails the suite instead of silently reusing a
stale PASS.

One rule cuts across all of them: a contract-test gate always reads its own
implementation script and its own test file, because those two files *are* the
gate. Auditing only the repo files a suite opens at runtime misses them, and then
a change to either skips the suite that would have caught it — the gate reports
PASS having never run.

| Scope | Required gates |
|---|---|
| Every change | Whitespace, staged/working consistency, shared agent rules, docs impact against base, README drift, process-spawn discipline, docs contract tests, ESLint |
| Contract-test inputs only | agent-infra contract tests, README drift tests, lint violation tests and fixtures — when a changed path is one those gates read |
| Frontend | TypeScript + desktop/mobile builds, bundle budget, all Vitest unit/integration tests, Playwright verify-smoke |
| Android | APK and instrumentation compilation, executed JVM tests, strict Android lint through `scripts/check-android.mjs`; device instrumentation runs separately |
| Rust | Fresh mobile build (or frontend build), Rust formatting, all-targets Clippy, locked Rust tests (the CI shards as concurrent multi-threaded processes, `scripts/rust-test-shards.mjs`), generated-binding drift |

Cargo runs inside `src-tauri` so its binding-export configuration applies.
The Rust test gate compiles once, then runs the CI shards, integration
binaries and doctests up to four processes at a time; each process runs its
own tests multi-threaded, which is safe because every DB-backed test installs
a private database for its own thread (issue #2048). The concurrency suites
assert a mechanism instead of a wall-clock budget (#2049). Verification
reruns failing tests alone as described below.

The shard runner copies each test executable out of the Cargo target before
running it. `src-tauri/build.rs` stamps the Buildmesh library binary with a
hash of the manifest directory, and the runner refuses a binary that does not
carry this worktree's stamp. A shared target, including a junctioned
`src-tauri/target`, can report another worktree's executable as fresh: Cargo's
metadata hash for a path package omits that path (issue #2339). The runner
then removes this repository's packages from the target (`cargo clean -p`,
including path dependencies such as `buildmesh_macros`) and compiles them
again. Registry dependency artifacts stay. `scripts/check.ps1` uses this
runner. A direct `cargo test` against the shared target can still execute the
other worktree's binary. Doctests stay on `cargo test --doc`, which holds the
target lock for that whole invocation.

A test process counts as finished when it exits, and its stdout and stderr are
collected for a bounded drain after that — the same cap `scripts/ci/run-guarded.mjs`
drains with, shared as `drainBounded`. A descendant that inherited those pipes
(a lingering `conhost.exe`, a stray cargo child) keeps them open past the child's
exit, and waiting for them left a process that was already gone reading as still
running (issue #2374). Reporting on `exit` settles a process the runner is no
longer waiting for; `tests/agent-infra/rust-test-shards.test.mjs` pins it.
Rust tests compile the desktop target as well as executing tests; this is a
compile smoke, not a packaged Tauri or real-window smoke. Playwright smoke uses
mock IPC. Visible UI or backend acceptance still requires the relevant real
dev-profile evidence from [verify-ui](../../.claude/skills/verify-ui/SKILL.md).

Vitest partitions the same unit/integration file patterns into the parallel
`jsdom` project and the `browser-tools` project. The four process-owning suites
listed in `vitest.config.ts` run after jsdom drains, one file at a time; this
also applies to the integration-only CI leg. Browser diagnostic string variants
are collector unit tests, with one real-browser warning case retaining the CLI
integration. Exact millisecond search benchmarks do not belong in the shared
correctness gate; Omnibar matching, ranking and limits remain covered there.

React Testing Library polls an async query for 1s before it reports a miss.
The suite gives that wait the same budget philosophy as its 30s per-test
budget: `tests/setup/vitest.setup.ts` sets a 10s async-query budget for the
suite's files, because on Windows the machine decides how long a render
takes. The last #2539 failure was that budget rather than a product defect —
the Agent Harness defaults reset button renders only after `list_providers`
lands, `get_app_preferences` delivers the stored default and the section's
re-key effect has flushed, and that wait measured 305-889ms inside a real
`npm run verify`, whose `frontend-tests` gate shares the machine with a
concurrent Rust compile. That is within 11% of RTL's 1s default, so one
more contention burst failed a test that passes in isolation. The ceiling
applies to the wait, not to the expectation: a query that is already
satisfied resolves on its first check, so a larger ceiling only widens the
window in which a slow render can still land.

Vite's ordinary optimizer cache lives in this worktree's `.tmp/vite`, outside
the potentially shared `node_modules`. Each `ui-shot --serve` child instead gets
a private directory under `.tmp/ui-shot-vite`, passed through
`BUILDMESH_VITE_CACHE_DIR`. Stopping an exited owned child removes its cache;
reusing an existing server does not take ownership of its cache or process.

That separation is not tidiness. Vite removes `<cacheDir>/deps` when it starts
with a config hash that differs from the cached one — it logs `Re-optimizing
dependencies because vite config has changed` — and that hash covers the
absolute root, so servers in different worktrees compute different hashes.
With Vite's default `node_modules/.vite` — one directory when the worktrees
share a node_modules junction — that is what happened: the second server
removed the dependency graph the first one was serving, and the browser
answered `504 (Outdated Optimize Dep)` instead of the app (#2515). A
worktree-local cache is not shared with a sibling worktree, and a private one
is not shared with another server; `tests/integration/ui-shot-server.test.ts`
reads the directory each server serves its dependencies from and asserts both.

The `ui-shot` mock renders charge their time to named phases in
`scripts/ui-shot-budgets.mjs`: dev-server startup, browser launch, page setup,
reading the mock fixtures, navigation, mount, the step script, importing the
steps module, the selector wait, the screenshot, the browser close, and the
dev-server stop. Reading the fixtures file and importing a steps module are
separate phases too, since both are awaited before the phase they feed.

A supervising wrapper needs only **one** deadline, and it is derived rather than
hand-summed: `UI_SHOT_WATCHDOG_DEADLINE_MS` is `UI_SHOT_WATCHDOG_MULTIPLE` times
the slowest per-phase budget, both computed in that module from the budgets
themselves. It replaced a 12-term sum of every phase (#2168). The sum was a
second copy of the phase list, and the copy — not the code — was the invariant:
nothing detected a phase present in neither copy, and keeping them in step cost
eight review rounds on the pattern that introduced it. Raising any phase now
widens the deadline with it, and no list is maintained twice.

A child-side watchdog in `scripts/phase-watchdog.mjs` supplies the diagnostic a
fixed deadline cannot. The child appends each phase name to a file as it enters
one, and the wrapper reads that file when it kills the child, so the report is
`ui-shot did not finish within 960000ms (killed while in phase: step script)`
rather than a bare timeout. The file is used rather than a stderr marker because
a killed child's pipes are lost with it; an appended file is already on disk.
`tests/integration/ui-shot.test.ts` asserts on the phases a real run emits
rather than auditing the script's source text, which could not see a phase in
neither copy and matched the first occurrence of a call string.

Phases Playwright cannot bound itself are bounded in `scripts/ui-shot-deadline.mjs`:
the step script, importing the steps module, reading the mock fixtures, page setup
(`browser.newPage` and `page.addInitScript`) — none of which accept a timeout
argument. A hanging step script is reported as the step script phase with the
offending steps file named.

Both `browser.close()` call sites are different: they are bounded by
`scripts/ui-shot-close.mjs`, which bounds how long the run *waits* without
letting a slow browser decide the run's verdict. The close resolves only once
Chromium has exited and Playwright has removed the profile directory it created,
which is work this repo does not own: measured on Windows it costs 2-3s on a
quiet host and 24-50s while a full suite saturates the machine, because
Playwright force-kills the browser it launched 30s into the close
(`DEFAULT_PLAYWRIGHT_TIMEOUT`) and the profile removal follows that. Racing that
at 15s and failing the run reported host contention as a product failure, for a
screenshot already written — the four `ui-shot` tests that carried this in
`scripts/known-flakes.json` each failed with that same close-timeout error
(#2245). An overrun is now reported on stderr and handed off;
`BROWSER_CLOSE_GRACE_MS` keeps the process alive to Playwright's own 30s kill
point so its profile cleanup runs in-process instead of losing to the exit
handler, whose synchronous removal runs against a browser that has not finished
dying and fails with EPERM (each occurrence observed under load left a
`playwright_chromiumdev_profile-*` directory behind; the four load runs after
this change left none). A close that *errors* is still the run's failure: slow
teardown is host contention, a broken one is not.

The step budgets and the teardown deadlines apply to `--mock` only. `--url` and
CDP-attach drive a real app with no supervising wrapper and no priced budget, so
their step phases and teardown stay unbounded rather than being cut off by a
number nothing accounts for: they pass a null close budget and keep the
unbounded close they used before the close budget existed. Their navigation is
bounded, but by Playwright's own default rather than by a priced budget.

`startDevServer` decides whether the URL is already served with a TCP connect, not
an HTTP request, and waits for readiness afterwards. A live dev server whose first
response is slow would otherwise look dead to a probe and be duplicated on a port
already taken. The trade-off: something that holds the port but never answers HTTP
now fails after the startup budget instead of being treated as absent and spawned
over.

There is no established frontend formatter, so this harness uses ESLint and
Git whitespace checks rather than imposing a new formatting policy. Clippy's
existing warning backlog remains visible as `warningCount`; any warning in a
touched file fails. This is not a repository-wide zero-warning certificate.
Rust formatting is stricter, because the crate is rustfmt-clean (#2022):
`cargo fmt --all --check` runs over the whole crate and **any** diff fails,
not only one in a touched file. From the repository root, run
`node scripts/rustfmt-touched.mjs <file.rs>...` with the files named in the failure,
then commit formatting alone as `style(rust): format <files>`. The helper restores
other tracked and non-ignored Rust files, including child modules that rustfmt
rewrites. Its named-file scope also matches the formatter hook; bare `rustfmt`
and writing `cargo fmt` commands are blocked because they can rewrite children
or the whole crate. The `guard-rustfmt.mjs` hook is an early warning: it denies a
`rustfmt` or `cargo fmt` command that is not `--check`, but cannot see a
command built at run time. A rustfmt failure that reports no diff (for example
a parse error) stays red. Any other existing failure stays red too: reproduce
at the recorded base before attributing it to baseline debt.

Checks run under deadlines using the existing process-tree guard. Output goes
to `.harness/logs/`; agents receive gate names, results, counts and log paths.
Child environments use `NODE_ENV=production` for frontend/mobile build gates
and `NODE_ENV=test` for test gates, clear `BUILDMESH_PREFILL` and `FORCE_COLOR`,
and use `NO_COLOR`. The outer npm/Node process may still warn if
its parent shell exports both color variables; clear `FORCE_COLOR` there too.
Building under the test environment includes development React code and makes
the production bundle budget check the wrong artifact.

A gate chooses its own environment by declaring `nodeEnv: 'production'` in the
plan (`scripts/harness-plan.mjs`); a gate without that property runs with the
harness-wide `test` value. The declaration travels with the gate rather than
living in a list of gate ids in the executor, so a new build gate is visibly
undeclared instead of quietly defaulting to the test environment. `scripts/check.ps1`
makes the same choice for its own `npm run build` and `npm run build:mobile`
calls, scoped to those invocations so its vitest calls still get the test
environment React's `act` requires.

On Windows that wrapper has to name npm's `.cmd` shim rather than bare `npm`,
because the two npm shims do not accept arguments the same way. `npm.cmd` takes
a real argument vector; `npm.ps1` rebuilds its own argument list by re-parsing
the *text* of the command that called it, so a forwarded `@args` splat is lost
and `npm run build` quietly becomes a script listing that exits 0. The wrapper
falls back to a bare `npm` only where no `.cmd` exists (pwsh on POSIX). A build
gate that prints a script list and reports success has built nothing: treat a
missing `dist` or a stale-bundle budget failure as this symptom first, not as a
build failure.

| Outcome | Meaning and next action | Exit |
|---|---|---|
| PASS | Every required gate passed, including executed tests | 0 |
| FAIL | Compiler, assertion, lint, drift or source-change failure; inspect the log and repair | 1 |
| FLAKY | Failed in the suite, passed alone; unlisted tests block completion | 1 |
| BLOCKED | A known prerequisite or command is unavailable; repair the environment or hand off with the reason | 2 |
| TIMEOUT | Deadline exceeded; inspect for code hangs and resource contention before retrying | 124 |

Unexpected nonzero commands are FAIL, not automatically attributed to the
environment. The first nonpassing gate stops the attempt (gates already running
in the other lane finish); unrun gates remain absent and cannot count as green. Receipt JSON preserves the command, base,
HEAD, worktree, paths, outcome, count, duration and log reference. Its content
fingerprint includes tracked/untracked inputs, tests, harness configuration,
lockfiles, HEAD, index content, tool versions and the run environment, which the
receipt records alongside the tree it hashed. Staged paths must match the
working content being tested. Source changes during verification invalidate
the result. Passing gates are reused across runs until the inputs they read change,
keyed on each gate's input set, environment identity, and task ID. Gates with no
explicit ignores read the whole tree. Completion continues to require that every planned
gate has a PASS record in the receipt for the current tree.

### Named test failures and isolation

Frontend tests use Vitest JSON; Rust shards record cargo/libtest failure ids and
their library, binary, integration or doctest target. Verify and `harness wait`
print up to ten names; the receipt retains every failure and the original log.
Once both lanes have drained, verify reruns each failed test once, sequentially:
one Vitest file with an escaped, anchored test-name pattern and one worker (the
Node API constrains discovery because CLI file filters match substrings), or the exact
Cargo library, binary or integration test in its original target with
`--test-threads=1`. Doctest failures remain named and FAIL with an explicit
"Doctest isolation unavailable" reason: rustdoc does not reliably select one
exact doctest on Windows. Each attempted rerun stores its command, log, executed
count, exit code and duration. Reruns have a five-minute individual limit and
a shared ten-minute deadline starting at the first diagnosis. Each rerun is
limited to the remaining budget; after exhaustion, remaining tests are TIMEOUT
and explicitly reported as not isolated. Neither the full
suite nor other gates run again to diagnose a failure.

A repeat failure stays FAIL. An isolated pass is FLAKY. Missing executables and
deadlines stay BLOCKED and TIMEOUT; zero executed tests, unreadable reports,
ambiguous duplicate names, file setup failures and unhandled runtime errors
cannot produce an accepted flake. Collection/setup failures still name the
failed file.

Runtime errors are attributed from Vitest's own reporting structures — the
`Errors  N error(s)` summary row, an `Unhandled Error(s)`/`Unhandled
Rejection`/`Uncaught Exception` banner, and its `Vitest caught N unhandled
error` sentence — rather than from the bare phrase appearing anywhere in the
output. The default reporter prints a test's title verbatim in the slow-test
listing and in the `stderr | file > suite > title` prefix, so a *passing* test
named after a runtime error would otherwise fail an otherwise healthy gate with
"Additional suite/runtime failures remain" (#2204). A test title is repository
data and is not reliably told apart from a diagnostic by wording; naming a test
after the failure it prevents is safe.

Git is checked before product test gates. On Windows, Rust compilation checks
network access to the pinned ConPTY package when its archive is absent from the
worktree cache;
an unavailable download is BLOCKED before Cargo builds. Cached archives do not
require network access. The Windows Rust runner checks
the staged ConPTY runtime after compilation, before starting test processes.
The [ConPTY unit test](../../tests/unit/conpty-runtime.test.ts) builds its own
fixture. The separate [app-version test](../../tests/unit/app-version.test.ts)
compares app manifests with local Git tags. Neither requires a blanket network
or installed-runtime exemption.

[`scripts/known-flakes.json`](../../scripts/known-flakes.json) maps exact ids to
open issue numbers. Vitest ids are `relative/file > full test name`; Rust library
ids are fully qualified names, and other targets use `kind:target > test name`.
The known-flakes gate and agent-infra tests validate that linked issues are open
GitHub issues. Every verify, including docs-only plans, performs this fresh
check before product gates; the result is never cached. Set `GH_TOKEN` or
`GITHUB_TOKEN` to authenticate and avoid the lower unauthenticated API limit.
Offline access or API rate limiting makes the check BLOCKED and stops the
entire verification plan. A closed issue fails validation and names the issue
in the gate reason. Only listed tests that actually pass alone allow subsequent
gates and `finish` to proceed. The gate row remains FLAKY with the issue link, while
the overall receipt is PASS if every gate is acceptable. These rows are never
cached. Persistent failures remain blocking even when listed. For an unlisted
flake, file an issue with both logs and add its exact id rather than retrying the
whole suite or changing an unrelated test.

`finish` launches no tests. It requires all planned gates, current source,
one current evidence entry per criterion, a current independent review entry,
and no unresolved blockers. Evidence prose is an agent/reviewer assertion;
the harness cannot prove a natural-language criterion or a reviewer's honesty.
Green checks do not replace semantic review.

## Hooks and guardrails

Claude SessionStart injects bounded Git/task context. PostToolUse performs
asynchronous changed-file lint and source type checks plus the portable agent
rules; unchanged fast evidence is reused. Existing edit/commit guards remain.
PreToolUse protects direct edits of `.harness` state; use the CLI instead.
Stop checks the current receipt and evidence rather than rerunning expensive
suites at every turn. A task whose phase is already `complete` is not
re-checked, so a later run that invalidated its recorded evidence does not
re-block finished work; start a new task for new work instead. A first
nonpassing stop presents the diagnostic. A recursive BLOCKED/TIMEOUT stop
permits an incomplete handoff. Once recorded
blockers exist, the very first stop of every later turn is released, so
the diagnostic is not repeated once per turn. A FAIL is never
released by a written report, however many times it is repeated. For a failed
implementation that cannot be repaired, write
`{"phase": "blocked", "blockers": ["<why>"]}` to a JSON file and run
`npm run harness -- update --spec <file>`; the FAIL stop message repeats this
command. Never edit `.harness/active-task.json` by hand. None of these paths
marks the task complete. A stale receipt names the run identity that differs
when there is one, so a block caused by a different `CARGO_TARGET_DIR` reads as
an environment mismatch rather than as a check the agent forgot.

These hooks apply to Claude; other agents use the portable CLI and existing CI
gates. Tasks must be started explicitly for this completion guard to apply;
read-only sessions are unaffected. Hooks are not a security sandbox: shell
writes can bypass direct-edit protection, and agents can modify local files.
Do not disable hooks or edit a receipt to obtain green. CI independently tests
the harness through `test:agent`; it does not trust local receipts.

Task operations serialize with `.harness/lock`; advisory fast checks use
`.harness/fast-lock` and publish only if the tested tree stayed current. An
operation reclaims a lock whose recorded PID is no longer alive, so a killed
run (a `verify` past the tool cap, for example) does not need manual cleanup;
the reclaim is recorded as a `lock-reclaimed` event. A lock that cannot be
read, or whose owner is still running, is left in place: inspect the PID
recorded there before removing it by hand. Otherwise resume
the existing task and rerun verification. State survives context resets and
process restarts, but deleting the worktree deletes its local continuity data.

## Checkpoints, evaluations and feedback

```powershell
npm run harness -- checkpoint
npm run harness -- record-rollback --ref <recorded-ref>
npm run eval:harness
npm run eval:behaviour
npm run harness -- evaluate --case terminal-mesh-switch
npm run harness -- metrics
```

`checkpoint` requires a clean worktree, creates a task-specific Git ref pointing
to committed HEAD, and records it in the task. It does not snapshot uncommitted
edits or `.harness` state. `record-rollback` verifies that clean HEAD matches a
recorded checkpoint after an operator has restored it; it never resets a
branch or discards files. Checkpoint refs remain recovery anchors until
explicitly removed by the operator. These new developer refs are separate
from product Circuit evidence checkpoints; the old product Git-checkpoint
table was removed.

[harness-corpus.json](../../scripts/harness-corpus.json) names seven important
product scenarios, the existing test selectors, evidence boundaries and
remaining live-runtime checks. The harness evaluation suite exercises actual
Git repositories/worktrees, task/receipt isolation, checkpoint recovery,
outcomes, zero-test rejection, hooks and stale evidence. The product corpus
executes targeted existing tests, not a coverage quota. Cargo cases require
fresh mobile assets (`npm run build:mobile`) and Tauri dependencies. A passing
case proves its stated boundary; it does not erase its remaining runtime gap.

Append-only `.harness/events.jsonl` records task/model identity when supplied,
verification attempts, individual gate outcomes/durations, progress phases,
checkpoints, recovery and evaluation results. Explicit phase transitions also
accumulate elapsed time in understand/plan/implement/verify/review phases;
completion records those phase durations. `metrics` aggregates outcomes,
verification time, failed gates, phase durations and recovery use. Phase and
task elapsed time include idle time; they do not measure active CPU work or
tokens. Native token counters and causal first-pass improvement metrics
are future work. Promote repeated failures into captured fixtures or an
executable regression, using the [audit](harness-audit-2026-10-02.md) as the
initial evidence. Avoid adding every incident to always-on instructions.
