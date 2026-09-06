---
name: verify
description: Verify a Buildmesh change with scoped build, test, and runtime evidence; report failures and coverage gaps accurately.
---

# Verify a Buildmesh change

Read `docs/agents/engineering.md` for the shared verification contract. Establish the change's base commit and acceptance outcomes before choosing checks. A user request to verify does not authorize unrelated repairs or publishing.

## Tiers

Default to standard; narrow to affected layers for documentation-only or frontend-only changes and state the scope.

- **quick:** Run `npm run test:agent`, `npm run check:agent -- --base <base>`, and focused behavior tests. For frontend changes run `npm run build`; for Rust changes compile the affected target with mobile assets built. A compile-only result is not Rust test coverage.
- **standard:** Run the scope-appropriate commands in the engineering contract (`scripts\check.ps1 all-ts`, `rust`, or `all` on Windows). For Rust changes also run `cargo clippy --locked --manifest-path src-tauri/Cargo.toml -- -D warnings`; ensure zero new compiler warnings on touched files and inspect generated binding changes when wire types change. Add `npx playwright test --project=verify-smoke` for terminal/browser behavior.
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
