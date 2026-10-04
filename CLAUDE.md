Buildmesh is a Tauri 2 desktop app (React 19, Rust) for orchestrating AI coding agents (Anthropic, Minimax, Kimi, OpenCode, Antigravity, Codex) across repositories, with persistent xterm.js terminals and hybrid Windows/WSL support.

## Start of every task
1. Check `git status --short` and the branch; record the base commit. Inside a worktree, every path you edit must be under the worktree root (see *Worktree path discipline* below).
2. Read `docs/agents/engineering.md` (seams, scope-based checks, evidence). For a cross-cutting change (new harness/provider, Tauri command, HTTP route, user-visible feature) follow the matching list in `docs/development/README.md#common-change-checklists`.
3. Read only the architecture you need. `docs/knowledge-primer.md` is ~150 KB, so **never read it whole**: run `rg -n "^#{2,3} " docs/knowledge-primer.md`, then Read the relevant section with `offset`/`limit`, and confirm it against the owning module (the code wins when they disagree). Domain language: `CONTEXT.md`. Rationale: `docs/adr/*.md`.
4. Verify with `npm run verify` (scope-selected gates); iterate with the focused commands below.

## Commands
- **Development harness:** `npm run harness -- start --spec .tmp/task.json`, then `npm run verify` and `npm run harness -- finish`. Resume with `npm run harness -- status`. Receipts, evidence and blocked handoffs: `docs/agents/development-harness.md`. `.harness/` is ignored worktree-local state; never hand-edit receipts.
- **Windows/worktree:** `scripts\check.ps1 [unit|integration|rust|docs|all-ts|all]`. `docs` runs the documentation contract without a build; `all-ts` builds and tests the frontend; `all` also runs Rust.
- **Agent infrastructure:** `npm run test:agent`; `npm run check:agent -- --base <base-commit>` checks changed source (committed work included) against the shared hook rules.
- **Documentation:** `npm run test:docs` (doc tests + commit guard); `npm run check:docs` (required pages, headings, links/anchors, source-of-truth drift).
- **Lint:** `npm run lint` (ESLint + React Hooks, `--max-warnings 0`, over `src/` incl. `src/mobile/`, `tests/`, `scripts/`); `npm run lint:fixtures` proves the hooks rules still fire on `tests/lint-fixtures/`.
- Test: `npm test` (unit + integration) · `npm run test:e2e` (needs app on :1991) · focused: `npx vitest run <file>`
- Typecheck/build: `npm run build` (`tsc`, desktop `vite build`, mobile `vite build --mode mobile`)
- **Rust** (inside `src-tauri/`): `cargo test` / `cargo clippy --all-targets`. A no-op `cargo check` does not replay a cached crate's warnings, so it can read as clean when it isn't. Strict `-D warnings` is still red repo-wide (issue #1491); touched files must have zero warnings. Whole suite: `node scripts/rust-test-shards.mjs` (runs the CI shards as a few concurrent processes; much faster than one serial `cargo test`). Wall-clock-budget tests can fail under CPU load (#2049): rerun with `BUILDMESH_RUST_TEST_JOBS=1` before attributing a failure. Iterate on one module with `cargo test --lib <module>:: -- --test-threads=1`.
- **Rust DB tests are not parallel-safe** (one process-global DB): use `--test-threads=1` for a trustworthy verdict. `db::circuit_tests` uses per-test in-memory DBs (parallel-safe).
- **Frontend test env:** run Vitest with `NODE_ENV=test` (`NODE_ENV=production` makes every render test fail with `React.act is not a function`). A fresh worktree has no `node_modules` and no `dist/mobile/`: from the worktree root run `New-Item -ItemType Junction -Path node_modules -Target ..\..\..\node_modules` (or `npm install`), and `npm run build:mobile` before `cargo test`.
- **Preserve mixed line endings.** Some files mix CRLF and LF (`git ls-files --eol | Select-String mixed`), e.g. `src/lib/tauri.ts`, `docs/knowledge-primer.md`. An edit can renormalise the whole file into a phantom diff: compare `git diff --stat` with `git diff --stat --ignore-cr-at-eol` before committing. If it happened, `git checkout -- <file>` and re-apply the change as a byte-level in-line replacement (`[System.IO.File]::ReadAllBytes` → Latin-1 round trip → `WriteAllBytes`).

## Hard rules — cause real breakage, do not violate
- **Never** call `.dispose()` on an xterm.js terminal unless the agent node is deleted → permanent terminal blanking. `TerminalManager` is a singleton; instances survive React remounts.
- **Never** pass Linux/WSL paths to Windows-side APIs. Convert via `env::to_host_path` (in `src-tauri/src/env/host_path.rs`, the `HostPath` sub-module); build `\\wsl$\` paths only inside that **module** (`HostPath`).
- **Never** acquire a DB connection in nested calls. Use `_inner(&Connection)` helpers; public read fns check out `read_conn()` (or `try_read_conn()` on async paths), public mutations lock `write_conn()`, and each passes one connection through (`src-tauri/src/db/mod.rs`).
- **Never** perform filesystem I/O, subprocess spawns, or network calls while holding a DB connection or writer mutex → deadlocks and blocks all concurrent DB operations. Use the three-phase pattern: prepare under lock, perform I/O lock-free, batch-write under lock (issue #1228).
- Don't replicate PTY-side `session-id` capture or node auto-naming — backend-only (`session_naming.rs`).
- New `#[command]` Tauri commands must be added to the `lib.rs` handler list, or they fail with "command not found" at runtime. Invoking Tauri core window/app APIs (e.g. `destroy()`) requires granting the corresponding capability in `src-tauri/capabilities/` or production calls fail silently (issue #1501).
- **Enforce invariants inside the state transition setter, not in caller wrappers.** If state A requires state B (e.g. viewMode 'all' requires null mesh), enforce it directly inside the store setter (e.g. `setViewMode`). Convenience helper wrappers (`showAllNodes`) leave secondary entrypoints (keyboard shortcuts, omnibar) desynchronized (issue #1002).
- **Fallbacks must never gate or discard live paths.** Offline/disk fallbacks run only when live requests fail or are absent; a missing fallback file must never discard an already-successful live network probe (issue #1073).
- **Never** hand-declare a TS interface for a Rust wire type (Tauri `invoke` or mobile HTTP). Derive `TS` on the Rust struct and import the generated type from `src/types/generated/` (regenerated by `cargo test`, drift-gated in CI). Annotate 64-bit ints with `#[ts(as = "i32")]`. Generated files are committed and never hand-edited. See *Shared Rust↔TS Types* in `docs/knowledge-primer.md` (issue #359).
- **Agent spawn shells are adapter-owned.** Each harness adapter's `spawn_recipe` (`src-tauri/src/agent/provider/adapters/<id>.rs`) declares its `WindowsShell` (`PowerShell`, `Cmd` for `.cmd` shims, or `Direct`), and `spawn_environment::wrap` consumes it. Read the adapter; never hard-code a shell elsewhere. On macOS/Linux every provider spawns `Direct`.
- **Worktree path discipline (Windows).** Inside a worktree (cwd under `.claude/worktrees/<name>/`), the file tools write the `file_path` you give them *verbatim* — an absolute path into the main checkout silently edits the wrong branch and your tests green against an unchanged tree. Always target paths under the worktree root. After any Edit/Write, trust `git --no-pager diff --stat` (only git/Bash see real disk; Read/Grep can read a phantom cache).

## Hooks (early warning only; CI is authoritative)
Claude hooks catch a subset of mistakes; their deny messages say how to proceed. Shell-tool writes bypass the edit hooks, so `git diff` is the evidence.
- `guard-antipatterns.mjs` (Edit/Write): blocks `.dispose()`, hand-built `\\wsl$\` paths, PowerShell `if`/`while` conditions on a native command (tests output, not exit code), and edits outside the session's worktree. Per-line escapes are named in the deny message.
- `guard-commit-staging.mjs` / `guard-documentation.mjs` (Bash and PowerShell): deny an empty plain `git commit`, and a behavior-sensitive commit without staged docs or a `docs: none — <reason>` line in the message.
- `verify-edit-persisted.mjs` checks modification time after an edit, not content.

## Code quality
- Match existing patterns. No new abstractions, deps, or speculative generality beyond the task.
- Add or update tests for behaviour changes (`tests/unit`, `tests/integration`).
- **Documentation is part of the definition of done.** User-visible behavior, configuration or shortcut changes, provider/harness or platform support, security/remote-access behavior, and public APIs require the relevant documentation page. Do not edit release notes per PR (`npm run release:notes` drafts them from Conventional Commits at release time), so write commit messages a reader could turn into a release-note line. If no update is needed, put `docs: none — <reason>` in the commit message. Run `npm run check:docs`.
- Test production boundaries and failure/order transitions, not copied logic or mock expectations. Runtime errors and zero executed tests are not green; report compilation, tests, and real/mock runtime evidence separately. No paper-tiger tests: do not short-circuit test bodies on OS/env to bypass assertions; assert literal outputs, not tautologies.
- Clean compiler and linter output: zero new warnings in touched files. Commit message claims must strictly match the diff.
- Comment only non-obvious *why*; let names carry the *what*.

## Pointers
- Design system: `DESIGN.md`. `src/App.css` `@theme` is the source of truth (dark + light); mobile mirrors it in `src/mobile/styles.css` `:root`. No hardcoded colours in components.
- Doc boundaries: `docs/knowledge-primer.md` holds durable architecture only (no release narratives, speculative rules, or line numbers); `CONTEXT.md` holds ubiquitous language only (no code symbols, paths, store keys); README.md is user-facing only (no issue numbers or backlog).
- DB schema: `src-tauri/src/db/mod.rs` (`SCHEMA_VERSION`); tables `meshes`, `agent_nodes`.
- Verification: `/verify` (`.claude/skills/verify/SKILL.md`). UI changes: `/verify-ui` drives the real dev-profile window (Playwright over CDP) for before/after PR screenshots.
- Shared entrypoints: `AGENTS.md` points here; `.agents/skills` points to `.claude/skills`. If Windows checks out a pointer file instead of a symlink, read its target explicitly. Edit canonical files, preserving the links.
- Probe dock tabs: `docs/development/probe-ui-checklist.md` (scroll ownership, 240px narrow width, status language, disclosure).
- Issues (`alondero/buildmesh`): `docs/agents/issue-tracker.md`; triage labels: `docs/agents/triage-labels.md`.
