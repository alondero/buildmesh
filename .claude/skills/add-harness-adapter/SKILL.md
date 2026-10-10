---
name: add-harness-adapter
description: Add or change an agent harness adapter. Use when adding a Provider variant, a spawn recipe, harness capabilities, or attention-hook support.
---

# Add a harness adapter

The adapter owns the spawn recipe, including `WindowsShell`: `PowerShell`, `Cmd` for a `.cmd` shim, or `Direct`. `spawn_environment::wrap` in `src-tauri/src/agent/spawn_environment.rs` reads that declaration. Do not hard-code a shell in the spawner. On macOS and Linux every provider uses `Direct`.

1. Add one file under `src-tauri/src/agent/provider/adapters` implementing `AgentProvider`. The trait is `src-tauri/src/agent/provider/mod.rs`. Register the module and its static in `src-tauri/src/agent/provider/adapters/mod.rs`.
2. Add the `Provider` variant and the `adapter()` arm in `src-tauri/src/models/agent.rs`. Add the id to `BUILTIN_HARNESS_IDS` in `src-tauri/src/agent/provider/mod.rs`. `claude` is an alias of `anthropic` via `HARNESS_PROFILE_ALIASES` in `src-tauri/src/agent/harness_catalog.rs`, not a second provider.
3. Add an `inspector_label` arm in `src-tauri/src/agent/harness_catalog.rs`. The match is exhaustive. `capabilities()` on the adapter is the source of truth. `builtin_harness_catalog()` writes the table. Do not hand-write a TypeScript capability map.
4. Declare `circuit_observation()` for the observation the harness really supports. The default is unwired. Do not add a harness-name branch under `src-tauri/src/services/circuit_worker`. The contract is `docs/development/circuit-session-observation.md`.
5. Cover a fresh spawn and a resume. When a test runs a real CLI, record that CLI's version next to the assertion.
6. Run `cargo test` from `src-tauri/` and commit `src/types/generated/HarnessCapabilitiesTable.ts` and `src/types/generated/HarnessCapabilitiesTable.json`.
7. Update the harness row in `docs/user-guide.md`. Add a caveat to `docs/troubleshooting.md` when runtime behaviour needs one. Touch `README.md` only when the user-facing harness list changes. Put externally verified CLI notes in `docs/learning` with a source link, not in the user guide as an assumption.

From `src-tauri/`: `cargo test --lib harness_catalog`. Then, from the worktree root, `npm run check:docs`.
