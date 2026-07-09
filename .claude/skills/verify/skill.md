---
name: verify
description: Autonomously verify a Buildmesh code change works — tiered build/lint/test/launch/log-scan loop with hill-climb on failure
---

# `/verify` — Autonomous verification for Buildmesh

## When to use

The user invoked `/verify`, `/verify quick`, `/verify standard`, `/verify full`, or `/verify --escalate`. Also activate when the user says "verify this works", "confirm the change is good", or asks you to run the full check before declaring a task done.

Distinct from `/use`: `/use` is a human-in-the-loop launch-and-watch. `/verify` is autonomous — it runs checks, fixes what fails, and returns a pass/fail report.

Sibling skill: if the change is **visible in the UI**, follow up with `/verify-ui` — it drives the real dev-profile window via Playwright-over-CDP and captures before/after screenshots for the PR.

## Tiers

Default tier is **standard**. Parse the first argument:

| Argument | Tier |
|---|---|
| (none) or `standard` | `standard` |
| `quick` | `quick` |
| `full` | `full` |
| `--escalate` | run `quick` → `standard` → `full`, stop at first failure |

### `quick` (~30–90 s)

Run in order; on any failure, enter hill-climb (see below). All steps must pass to call the tier green.

1. `npm run build` — runs `tsc` then `vite build`. tsc errors out on type issues; vite catches missing imports.
2. `cargo build --manifest-path src-tauri/Cargo.toml` — Rust compile.
3. `npm run test:unit` — Vitest unit suite (118+ tests, mocked Tauri).

### `standard` (~2–3 min) — default

1–3 above, plus:

4. `cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings` — Rust lint with warnings-as-errors.
5. `npm run test:integration` — Vitest integration suite (uses mocked `invoke()`; **does not** require a running Tauri instance, so safe in this tier).

### `full` (~5–8 min)

1–5 above, plus:

6. `npm run tauri:build:dev` — produces the **dev profile** bundle at `src-tauri\target\release-dev\release\buildmesh-dev.exe` (identity `com.alond.buildmesh.dev`, ports 2991/2992). The dev build sets `CARGO_TARGET_DIR=src-tauri\target\release-dev` (via `scripts\run-dev.ps1` on Windows / `scripts\run-dev.sh` elsewhere) so it lands in a separate target dir from the stable hub and never file-locks the hub's `buildmesh.exe`. Cargo nests the profile subdir, hence the `release-dev\release\` two-level path.
7. Launch the app via `scripts\run-dev.ps1`. The script kills any existing `buildmesh-dev` process (**never** the stable hub), verifies the binary, records the pre-launch log line count, starts the binary, and confirms startup.
8. **Strict log scan** of `%APPDATA%\com.alond.buildmesh.dev\logs\buildmesh.log` against the line count captured before launch — see rules below.
9. `npm run test:e2e` — Playwright e2e. Playwright's `webServer` config (`playwright.config.ts`) boots its own `npm run tauri dev`, which uses the **base** identity and ports **1991/1992** (not the dev profile). If a stable hub is running it holds 1991, so pause the hub for this step or run e2e separately. The dev exe launched in steps 6–8 uses 2991/2992 and does not collide with Playwright's dev server.

## Hill-climb protocol

On any failing step:

1. **Capture the failure shape.** First error line; file path and line number if present; error code (`E0382`, `TS2345`, `LNK1107`, etc.); panic message; first failing test name.
2. **Check Known failure modes below first.** If the failure matches a recipe, apply the recipe directly and re-run the same tier from step 1. Don't re-derive what the recipe already encodes.
3. **Otherwise diagnose normally.** Read the failing file, find the cause, edit, re-run the same tier from step 1.
4. **Iteration cap: 5 per tier.** If still failing after 5 fix attempts, **stop** and report to the user with: tier, which step failed, what was tried each iteration, the still-failing output.
5. **On success at the requested tier, stop.** Do not auto-escalate unless `--escalate` was passed.
6. **After any novel failure** (no matching recipe) that you successfully fix, **append a new recipe** to the Known failure modes section below, before declaring done. Format: `**Trigger pattern** → root cause. Fix: <one line>. Memory: <link if applicable>.`

## Strict log scan (full tier, step 8)

Log path: `$env:APPDATA\com.alond.buildmesh.dev\logs\buildmesh.log` (the dev profile launched in step 7).

Compute new lines only (lines added since pre-launch count from `run.ps1`). Scan those new lines:

| Pattern in new lines | Action |
|---|---|
| ` ERROR ` or `panic` or ` panicked at ` | **FAIL** |
| `Illegal invocation` | **FAIL** — Web API receiver binding regression. Memory: `buildmesh-webapi-receiver-binding` |
| `command .* not found` or `command not registered` | **FAIL** — Tauri command not in `lib.rs` invoke handler. Memory: `tauri-command-registration` |
| `Failed to spawn` or `error 193` or `0xc0000142` | **FAIL** — Agent spawn regression. Memory: `buildmesh-agent-spawn-regressions` |
| ` WARN ` (not in benign list below) | **REPORT** in the summary, do not fail |
| Nothing matched, process still alive | **PASS** the log-scan step |

Benign WARN list (seed; extend as you confirm a WARN is non-actionable):
- Tauri startup `IPC scope not set` boilerplate

## Known failure modes

Seed entries — pattern-match the failure against these first before diagnosing. Append new entries here when you fix a novel failure.

- **`error: linking with link.exe failed` / `LNK1107` during `cargo build` or `tauri build`** → A previous instance is still running and holds the output file. Fix: `Stop-Process -Name buildmesh-dev -Force` (dev-profile build) then retry once. A plain `cargo build` writes `buildmesh.exe`, so if the stable hub is running, build the dev profile instead (`npm run tauri:build:dev`) — it writes `buildmesh-dev.exe` and won't conflict.
- **PowerShell agent spawn fails with parse error referencing the prefill text** → BOM in base64 UTF-16LE payload to `-EncodedCommand`. Fix: strip BOM after encoding in `src-tauri/src/main.rs`. Memory: `buildmesh-powershell-encoding-fix`.
- **`Illegal invocation` thrown in WebView2 or silently swallowed by a Tauri listener** → Web API stored as a method (`this.x = requestAnimationFrame`) rebinds receiver. Fix: use arrow wrapper or `.bind(window)`. Memory: `buildmesh-webapi-receiver-binding`.
- **Tauri command works in code but `invoke('cmd_name', ...)` rejects at runtime with "command not found"** → `#[tauri::command]` macro alone isn't enough; the command must be added to the `invoke_handler` `generate_handler!` list in `src-tauri/src/lib.rs`. Memory: `tauri-command-registration`.
- **Terminal panes stack vertically when switching meshes / projects** → `detach()` is not removing `term.element` from its parent before reuse. Fix: explicit `element.parentNode?.removeChild(element)` in detach. Memory: `buildmesh-terminal-container-reuse-bug`.
- **Agent spawn fails with `--model` argument missing** → empty-string `model` (a blank `meshes.model` column, originally surfaced when the value came from a `mesh.toml` `[agent] model = ""` line) is being passed as `--model ""`. Fix: `.is_empty()` guard before adding the arg in the spawn code. Memory: `buildmesh-model-empty-string-bug`.
- **`gh pr create` fails with "No commits between X and X"** → current branch equals base ref, and `--head` was passed. Fix: skip `--head` when `current_branch == base_ref`. Memory: `buildmesh-pr-same-branch-fix`.
- **~20 `git::worktree` / `agent::spawn` / `commands::pr` tests fail with `failed to resolve path '/home/<user>/AppData/...'`** → a non-Git-for-Windows `git.exe` (e.g. devkitPro's MSYS2 git) is first in PATH and writes POSIX-style worktree gitdir paths Windows libgit2 can't resolve. Fix: run via `scripts\check.ps1` (pins `C:\Program Files\Git\cmd` first since 2026-07-09) or prepend it to PATH manually.
- **Playwright e2e times out waiting for `http://localhost:1420`** → a stale `tauri dev` process from a previous run is bound but unhealthy. Fix: `Stop-Process -Name node -Force` (kills Vite) and `Stop-Process -Name buildmesh -Force` (kills the dev exe), then re-run.
- **`vitest run tests/integration` fails with "ECONNREFUSED 127.0.0.1:1991"** → an integration test is hitting the HTTP test bridge instead of the mocked `invoke()`. Likely a test using `invokeViaHttp` from `tests/e2e/utils/tauri-http.ts`. Fix: that test belongs in `tests/e2e/`, not `tests/integration/`; move it or run it inside Playwright.

## Reporting

When you finish (pass or fail, including iteration-cap stop), produce a structured summary the user can scan in 5 seconds:

```
/verify <tier> → <PASS | FAIL @ step N>

Quick:    ✓ build  ✓ cargo  ✓ unit
Standard: ✓ clippy ✓ integration
Full:     ✓ tauri build  ✓ launch  ✗ log scan
          └─ "ERROR rusqlite: UNIQUE constraint failed" at line 1247

Fixes applied this run:
- <one line per fix>

WARN (informational):
- <each warn line, or "none">
```

## Notes

- Always use `npm run tauri build` for the full bundle — never `cargo build --release` directly. The Tauri CLI handles frontend → backend wiring; bypassing it produces an exe with stale frontend assets. Memory: `feedback_frontend-bundling`.
- This skill takes precedence over the global `/verify` skill because it lives in `.claude/skills/`. The global skill's generic "figure out how to run the app" is replaced here by the Buildmesh-specific pipeline.
- The skill file is meant to grow. When you append a new Known failure mode, keep entries one-line and pattern-first so future pattern-matching stays cheap.
