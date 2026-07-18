# ADR 0021 — In-app auto-updater and tag-triggered GitHub Releases

## Status

Accepted (2026-07-16)

## Context

Buildmesh had no distribution story beyond a raw CI artifact: `build.yml`
uploaded `src-tauri/target/release/bundle/**` on every push to `main`, and
`tauri.conf.json` configured neither an updater nor any signing. Two readiness
problems followed (issue #826):

- **No auto-update.** Every teammate re-downloaded the installer by hand and
  versions drifted apart.
- **Unsigned installers.** Windows SmartScreen ("unknown publisher") and macOS
  Gatekeeper ("unidentified developer") warn on first launch.

These are separable. OS **code signing** (Authenticode / Apple notarization) is
what silences SmartScreen/Gatekeeper — it costs money, needs identity
verification, and is deferred (see "Deferred" below). The **updater** is
independent: Tauri's updater has its own free minisign signing that only proves
an update package is authentic. We can ship auto-update now with zero paid certs
— the installers stay "unsigned" to the OS, but teammates stop drifting.

## Decision

1. **Wire `tauri-plugin-updater` + `tauri-plugin-process`.** The app checks a
   release feed on startup (production Tauri builds only) and shows an in-app
   "Install & Restart" prompt (`src/components/UpdatePrompt`). Install runs the
   plugin's `downloadAndInstall()` then `relaunch()`.

2. **GitHub Releases is the feed.** `plugins.updater.endpoints` points at
   `…/releases/latest/download/latest.json`. A new **tag-triggered** workflow
   (`.github/workflows/release.yml`, `on: push: tags: v*`) builds the Windows
   installer + updater artifacts via `tauri-apps/tauri-action` and publishes a
   (draft) GitHub Release containing the installer, its `.sig`, and the
   generated `latest.json`. It is deliberately **not** on every push — a signed
   multi-artifact build per commit is expensive.

3. **Signing split: base config off, release overlay on.** The committed minisign
   **public key** lives in `tauri.conf.json` (public keys are meant to be
   public, and the key is required at build time — an empty one fails the
   build). `bundle.createUpdaterArtifacts` — which triggers *signing* and needs
   the **private** key — is left **off** in the base config and flipped on only
   by `src-tauri/tauri.release.conf.json`, passed to the release build via
   `tauri build --config`. This mirrors the existing `tauri.dev.conf.json`
   overlay pattern and keeps the per-push `build.yml` green **without** a signing
   secret (critical: PR builds — including from forks — cannot see secrets).

4. **Windows-only releases, for now.** The app is Windows/WSL-centric; a
   cross-platform matrix would ~4x CI cost per tag and needs each OS to compile
   Windows-centric code. The base `tauri.conf.json` already narrows
   `bundle.targets` to `["msi", "nsis"]` (Windows-only installer formats), so
   `tauri build` on macOS/Linux would produce no installer today — extending
   the workflow to a matrix (issue #835) is also the right time to widen the
   target list. Tracked as a follow-up issue.

## Signing keys

Generated once with `tauri signer generate`. The **private** key lives only in
the maintainer's `~/.tauri/buildmesh.key` and in the `TAURI_SIGNING_PRIVATE_KEY`
GitHub Actions secret — never committed (`src-tauri/.gitignore` has a defensive
`*.key`). See `docs/development/releasing.md` for the one-time secret setup and
the release cadence.

## Deferred

- **OS code signing / notarization** (SmartScreen/Gatekeeper) — needs paid
  certs; the release workflow already has the seam to consume them. Tracked in
  issue #834.
- **macOS/Linux release builds** — extend the workflow matrix once non-Windows
  builds are validated. Tracked in issue #835.
