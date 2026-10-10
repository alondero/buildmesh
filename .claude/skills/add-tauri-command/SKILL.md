---
name: add-tauri-command
description: Add or change a Tauri command or the mobile HTTP route that shares it. Use when registering a command, a generate_handler entry, a Rust-to-TypeScript wire type, or a wrapper in src/lib/tauri.
---

# Add a Tauri command

Follow this in order. The rules it protects are in `CLAUDE.md` and `docs/development/rust-conventions.md`. A command validates input, calls one service or `db::` function, and maps the error to `String`.

Do not copy `src-tauri/src/commands/git.rs`, `src-tauri/src/commands/diff.rs`, `src-tauri/src/commands/pr.rs`, `src-tauri/src/commands/prune.rs`, `src-tauri/src/commands/ai_context.rs`, or `src-tauri/src/commands/build_run.rs`. They still open `git2` repositories. That debt is pinned by `tests/unit/git2-ownership.test.ts`.

1. Put the command in `src-tauri/src/commands/`. Blocking work (SQLite, `std::fs`, `preferences::load`, `preferences::save`, git, network) is a plain sync `*_blocking` core plus a `#[command]` `async fn` that calls `run_blocking` from `src-tauri/src/blocking.rs`. A fast in-memory command may stay a sync `#[command] fn`.
2. Register it in `tauri::generate_handler!` in `src-tauri/src/lib.rs`. A missing entry fails at runtime with "command not found".
3. If it calls a Tauri core window or app API, grant that capability in `src-tauri/capabilities/default.json`. A missing grant fails silently in a production build.
4. Wire structs derive `TS` with `#[ts(export, export_to = "Name.ts")]`. Annotate `i64`, `u64`, and `usize` with `#[ts(as = "i32")]`, and `Option<i64>` with `#[ts(as = "Option<i32>")]`. A list of ids on the wire is `Vec<i32>`. Run `cargo test` with working directory `src-tauri/` so `src/types/generated` updates. Commit the generated files. Never hand-edit them, and never declare a TypeScript interface for the same struct.
5. Put the desktop wrapper on the facet that already owns the area: `src/lib/tauri/provider.ts`, `src/lib/tauri/circuitBlueprint.ts`, `src/lib/tauri/circuitEvidence.ts`, `src/lib/tauri/history.ts`, or `src/lib/tauri/stateRecovery.ts`. Otherwise add it to `src/lib/tauri.ts`. Every wrapper calls `_invoke` from `src/lib/tauri/_invoke.ts`. Do not call `invoke` anywhere else.
6. A mobile route under `src-tauri/src/http/routes` awaits the async command wrapper, or calls `run_blocking` itself. It must not call the `*_blocking` core on the async worker.
7. Cover malformed input, a missing dependency, and acknowledged success at that command or route. The guards are `tests/unit/ipc-contract.test.ts`, `tests/unit/tauri-ipc-seam.test.ts`, and `tests/unit/async-command-blocking.test.ts`.

From the worktree root, with `NODE_ENV=test`:

`npx vitest run tests/unit/ipc-contract.test.ts tests/unit/tauri-ipc-seam.test.ts tests/unit/async-command-blocking.test.ts --pool=threads`

Rust behaviour: from `src-tauri/`, `cargo test --lib <module>`. Build `dist/mobile` first (`npm run build:mobile`) when that directory is missing.
