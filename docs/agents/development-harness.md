# Development harness

Current contributor and agent workflow. The repository harness records a task,
selects checks, and refuses a successful `finish` without current check,
acceptance, and review evidence. It complements Buildmesh's product Circuits;
it does not change their state machine or automatically publish anything.

## Start and resume

Run from the intended worktree with Git, Node/npm, and installed dependencies.
Rust checks also require Cargo and the Tauri platform dependencies. Browser
smoke requires `npx playwright install chromium`. Windows worktree setup is
documented in [CLAUDE.md](../../CLAUDE.md).

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

### Lanes and the machine-wide slot limit

Quick infrastructure gates (whitespace, shared rules, docs, README, process
spawns, contract tests, ESLint) run first, in order. The product gates then run
as two lanes at the same time, each keeping its own order and fail-fast:

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

Committed, staged, unstaged, deleted and untracked paths determine scope.
Unknown paths select both product layers; generated wire types always select
both. Executable hooks and harness tests still run infrastructure gates.
Adding only harness package scripts, or the exact `.harness` ESLint ignore,
keeps infrastructure scope; dependency/build/rule changes retain product gates.

| Scope | Required gates |
|---|---|
| Every change | Whitespace, staged/working consistency, shared agent rules, docs impact against base, README drift, process-spawn discipline, agent/docs/README/lint contract tests, ESLint, lint violation fixtures |
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
Rust tests compile the desktop target as well as executing tests; this is a
compile smoke, not a packaged Tauri or real-window smoke. Playwright smoke uses
mock IPC. Visible UI or backend acceptance still requires the relevant real
dev-profile evidence from [verify-ui](../../.claude/skills/verify-ui/SKILL.md).

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
the step script, importing the steps module, reading the mock fixtures, both
`browser.close()` call sites, and page setup (`browser.newPage` and
`page.addInitScript`) — none of which accept a timeout argument. A hanging step
script is reported as the step script phase with the offending steps file named.
The step budgets and the teardown deadlines apply to `--mock` only. `--url` and
CDP-attach drive a real app with no supervising wrapper and no priced budget, so
their step phases and teardown stay unbounded rather than being cut off by a
number nothing accounts for. Their navigation is bounded, but by Playwright's own
default rather than by a priced budget.

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
Rust formatting follows the same rule: `cargo fmt --all --check` runs over
the crate, a diff in a touched file fails, and the crate's existing
formatting backlog (#2022) is reported as `formatDiffCount` instead of
blocking every Rust change. Format touched files with
`node scripts/rustfmt-touched.mjs <file.rs>...`. Do not run `cargo fmt` (it
rewrites the whole crate) or bare `rustfmt <file>`: on a module root
(`lib.rs`, `mod.rs`) rustfmt also formats the child modules. The script
restores every other Rust file (tracked, or untracked and not ignored) byte
for byte. The `guard-rustfmt.mjs` hook is an early warning: it denies a
`rustfmt` or `cargo fmt` command that is not `--check`, but cannot see a
command built at run time. A
rustfmt failure that reports no diff (for example a parse error) stays red.
Any other existing failure stays red too: reproduce at the recorded base
before attributing it to baseline debt.

Checks run under deadlines using the existing process-tree guard. Output goes
to `.harness/logs/`; agents receive gate names, results, counts and log paths.
Child environments use `NODE_ENV=production` for frontend/mobile build gates
and `NODE_ENV=test` for test gates, clear `BUILDMESH_PREFILL` and `FORCE_COLOR`,
and use `NO_COLOR`. The outer npm/Node process may still warn if
its parent shell exports both color variables; clear `FORCE_COLOR` there too.
Building under the test environment includes development React code and makes
the production bundle budget check the wrong artifact.

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
lockfiles, HEAD, index content and tool versions. Staged paths must match the
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
failed file. Git is checked
before product test gates. On Windows, Rust compilation checks network access
to the pinned ConPTY package when its archive is absent from the worktree cache;
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
suites at every turn. A first nonpassing stop presents the diagnostic. A
recursive BLOCKED/TIMEOUT stop permits an incomplete handoff. Once recorded
blockers exist, the very first stop of every later turn is released, so the
diagnostic is not repeated once per turn. A FAIL is never
released by a written report, however many times it is repeated. For a failed
implementation that cannot be repaired, write
`{"phase": "blocked", "blockers": ["<why>"]}` to a JSON file and run
`npm run harness -- update --spec <file>`; the FAIL stop message repeats this
command. Never edit `.harness/active-task.json` by hand. None of these paths
marks the task complete.

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
