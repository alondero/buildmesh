---
name: add-setting
description: Add or change a user setting or preference. Use when editing AppPreferences, the settings UI, the resolver cascade, or a launch configuration.
---

# Add a setting

`AppPreferences` in `src-tauri/src/preferences/model.rs` is the wire struct. Import `src/types/generated/AppPreferences.ts`. Do not declare a parallel TypeScript interface.

1. Add the field on `AppPreferences` with a serde default so an existing preferences file still loads. A JSON shape change goes through `src-tauri/src/preferences/migrations.rs`. Load and save stay in `src-tauri/src/preferences/storage.rs`, off the async worker pool.
2. A value that changes how an agent spawns is resolved in `src-tauri/src/preferences/resolver/cascade.rs` (explicit, then mesh, then application). The spawn path and `get_resolved_harness_view` both use that cascade. Do not re-derive the order in a command or a React component.
3. A per-harness launch default (model, effort, extra arguments, permission mode) belongs in `src-tauri/src/preferences/launch_configurations.rs`, not a one-off field on the modal.
4. Put the control in the existing pane under `src/components/AppSettings`: `src/components/AppSettings/GeneralPane.tsx`, `src/components/AppSettings/HarnessesPane.tsx`, or `src/components/AppSettings/ProvidersPane.tsx`. Colours come from `src/App.css`. Do not hardcode a colour.
5. Document the default, what it changes, and how to undo it in `docs/user-guide.md`.

When the wire struct changes, run `cargo test` from `src-tauri/` and commit `src/types/generated`.

From `src-tauri/`: `cargo test --lib preferences`. For a visible control, also run the unit test that renders the pane you edited (`npx vitest run <file> --pool=threads` from the worktree root, with `NODE_ENV=test`).
