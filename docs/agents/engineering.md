# Engineering and verification contract

Read this for implementation and review. Architecture belongs in `docs/knowledge-primer.md`; domain names in `CONTEXT.md`. Apply the sections relevant to the change. This contract is shared across harnesses; Claude hooks are only an early warning layer.

## Before changing code

Confirm `git rev-parse --show-toplevel`, branch, and `git status --short`. Record the requested behavior and the base commit used for comparisons. Read the originating issue and available review findings when the task references them. Translate acceptance criteria into observable outcomes and identify the production boundary each test will exercise. A review finding is closed by code and evidence, not by rewriting the description.

## Design for evidence

| Change | Design seam and minimum useful regression evidence |
|---|---|
| Async UI, refresh, polling | Give requests an owner and define which result may commit. Use controlled promises to resolve old/new requests in both orders; cover rejection, unmount, and event versus polling reconciliation when applicable. Assert visible state and pending controls. Example: `tests/unit/usage-tab.test.tsx`. |
| Database evolution | Pass an explicit `&Connection` to schema logic; avoid initializing the process singleton just to test migrations. Run the production schema initializer against a fresh in-memory DB and a legacy schema with rows; assert preserved data, new columns/indexes, and repeat initialization. Example: `src-tauri/src/db/migration_tests.rs`, `init_schema_upgrades_legacy_circuit_runs_before_creating_the_queue_index`. |
| HTTP / IPC / provider integration | Keep adapters thin; test the actual route/adapter as well as pure parsing. Cover malformed input, unavailable dependencies, and acknowledged success. Client mocks cannot prove Rust registration, delivery, or CLI compatibility. Test fresh and resume paths for changed launch recipes; record the CLI version for live contract checks. |
| Worker / lifecycle / resource ownership | Keep decisions independent of SQLite, PTY, clocks, and network; inject observations at the existing seam. Test cancellation, restart/replay, stale completion, and cleanup failure where relevant. Assert durable state and effects together. Examples: circuit `stepper` and `services/circuit_worker` tests. |
| UI layout and gestures | Verify actual narrow layout (Probe: 240px), error/loading/empty states, and keyboard/touch behavior. Alert placement and accessibility-role changes are UI changes. Test rendered geometry in a browser; jsdom class assertions cannot establish fit or scrolling. Read `docs/development/probe-ui-checklist.md`. |

Use the smallest existing seam that hides the external dependency. Avoid exporting internals only to test them, reproducing production algorithms inside tests, empty tests, and mocks that merely echo the expectation. For a bug, demonstrate the regression fails before the fix when practical; otherwise explain why the test distinguishes the faulty behavior. Use promises/events/fake clocks for ordering rather than arbitrary sleeps. A no-console-warning assertion alone does not prove unmount safety.

## Choose checks by scope

| Scope | Commands / evidence |
|---|---|
| Instructions, hooks, verification scripts | `npm run test:agent`; `npm run check:agent`; run changed hooks through their stdin entrypoints and run affected script paths. |
| Frontend | `scripts\check.ps1 all-ts` on Windows; elsewhere `npm run build` and `npm test`. Focused tests are useful during iteration; report their scope. |
| Rust | `scripts\check.ps1 rust` on Windows. Elsewhere build mobile assets, unset `BUILDMESH_PREFILL`, then `cargo test --locked --manifest-path src-tauri/Cargo.toml`. Changed mobile source requires a fresh mobile build before Rust embeds it. |
| Frontend + Rust | `scripts\check.ps1 all` (build + unit + integration + Rust + agent checks). This does not run Clippy, binding drift, or browser tests. |
| Rust wire types | Regenerate via `cargo test`; inspect `git status --short -- src/types/generated` including new files and commit generated outputs. CI independently checks drift. |
| Terminal rendering / browser events | `npx playwright test --project=verify-smoke` (Vite + mock IPC; no backend proof). |
| Visible UI / real runtime | Read `.claude/skills/verify-ui/SKILL.md`; use the dev profile and functional assertions plus inspected screenshots. Full `chromium` e2e has additional runtime requirements; read the specs/config before running it. |

Vitest defaults to threads, rejects zero discovered tests, and fails on unhandled errors. Do not weaken assertions, ignore errors globally, or change test discovery to get a green result. `cargo check`, `--no-run`, and successful compilation are not executed tests. A filtered Rust invocation that ran zero tests provides no behavioral evidence.

## Evidence at handoff

Report the commands actually run, result, executed test counts, and relevant platform/runtime. Separate passed, failed, and not run checks. State whether browser evidence used mock IPC or the real backend. A failed command stays failed even if a subset passes. Call a failure pre-existing only after reproducing it at a recorded base with equivalent conditions; unrelated file paths alone are insufficient. If baseline comparison is unavailable, say attribution is unverified.

Before committing, inspect `git diff --check`, `git diff --stat`, and `git diff --cached --stat` against the intended changes. After committing, inspect the actual commit. Before handoff, run `npm run check:agent -- --base <recorded-base>` so already committed changes are covered. Keep scratch bodies/logs in ignored `.tmp/`; do not publish screenshots, PRs, or messages unless the task authorizes publication.

## Enforcement and maintenance

- `npm run check:agent` checks added lines in staged, unstaged, and untracked source files against HEAD. `--base <commit>` includes commits since that base. CI uses the PR base or push predecessor. It reuses `.claude/hooks/guard-antipatterns.mjs`; it does not alter the index.
- These are content heuristics: terminal-named TS files, Rust WSL UNC strings outside `env/`, and component radius tokens. They do not establish terminal ownership, path correctness, DB lock safety, command registration, or wire-type completeness. Per-line exceptions require a local reason. Renames are checked as additions; existing unchanged violations are not a baseline-wide gate.
- Claude `Edit|Write|MultiEdit` hooks do not cover shell writes or other harnesses. The persistence hook checks modification time, not content; `git diff` remains the evidence. The staging hook only catches some empty commits. CI also checks process-spawn patterns and generated binding drift; those gates do not replace behavior tests.
- `CLAUDE.md` is canonical; Git records `AGENTS.md -> CLAUDE.md` and `.agents/skills -> ../.claude/skills`. On Windows with symlink checkout disabled, these can be plain files containing the target. Follow that target and read `.claude/skills/<name>/SKILL.md` explicitly when discovery is unavailable; do not edit the pointer as if it were the instructions.
- Keep always-on instructions short. Add a durable lesson here or in the relevant architecture section only when it changes a future decision. Prefer an executable regression over another prohibition; keep historical evidence in the audit, not every skill. Changes to enforcement need allowed and denied cases plus real entrypoint tests.
