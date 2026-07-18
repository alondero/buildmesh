<!--
  GitHub auto-fills this template into every new PR body.
  Anything marked `<!-- optional -->` is fine to delete if N/A.
  The first unchecked checkbox that is meaningful for your PR should be
  the only thing left un-ticked — strike the rest or convert to `[x]`.
-->

## What

<!-- One or two sentences. State the user-visible change, not the implementation. -->

Closes #

## Why

<!-- Root cause / motivation. Reference the issue's bullets if there are any. -->

## How to test

<!-- Concrete steps the reviewer can run. If `npm run test:ci` covers it, say so and skip the manual steps. -->

- [ ] `scripts\check.ps1 all` is green on Windows
- [ ] `cargo test` is green (inside `src-tauri/`)
- [ ] (UI changes) `/verify-ui` was run; before/after screenshots attached below

## Triage

- [ ] PR title uses **Conventional Commits** (`feat(scope): …`, `fix(scope): …`, `refactor(scope): …`, `docs(scope): …`, `chore(scope): …`) — CI gates on this
- [ ] Files outside the worktree root were not edited
- [ ] No new `.dispose()` on a live xterm terminal; no hand-built `\\wsl$\` paths; no hand-declared TS interface for a Rust wire type (see `CONTRIBUTING.md` for the full rule list)
- [ ] New `#[command]` is registered in `src-tauri/src/lib.rs`
- [ ] (Touches schema) `cargo test` regenerated `src/types/generated/` and the diff is committed

<!-- optional: screenshots / screen recordings -->
<!-- optional: breaking-change notes -->
<!-- optional: backwards-compat / migration notes -->