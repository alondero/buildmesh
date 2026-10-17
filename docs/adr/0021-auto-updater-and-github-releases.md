# ADR 0021 — In-app auto-updater and tag-triggered GitHub Releases

## Status

Accepted (2026-07-16). Amended 2026-10-17 (issue #2438) — see [Amendment](#amendment-2026-10-17-the-updaters-zip-extraction-feature-stays-off). Decisions 1–3 are unchanged.

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
   Windows-centric code. The release job runs only on `windows-latest`, so what
   ships is a Windows installer regardless of `bundle.targets` — the base
   `tauri.conf.json` sets `"targets": "all"`, and the release overlay does not
   narrow it, but a non-Windows runner is never used to build a tagged release.
   Extending the workflow to a matrix (issue #835) is also the right time to
   widen the target list deliberately rather than inherit `all`. Tracked as a
   follow-up issue.

## Amendment (2026-10-17): the updater's ZIP extraction feature stays off

Issue #2438. This qualifies decision 4 rather than reversing it, and it adds a
security condition that was not recorded here.

### Context

`zip` is not a direct dependency. It reaches the tree solely through
`tauri-plugin-updater`'s **default** `zip` feature, which the manifest adopted
implicitly with `tauri-plugin-updater = "2"`. Three advisories against `zip` 4.x
— `RUSTSEC-2026-0367` (SUID/SGID preservation), `RUSTSEC-2026-0368` (symlinks
targeting paths outside the destination), `RUSTSEC-2026-0369` (Unix permissions
applied through unconfined symlinks) — are fixed only in 9.x.

There is no version to upgrade to. At the time of writing, every published
release of the plugin requires `zip = "4"`, so `cargo update -p zip --precise
9.0.4` fails to resolve. This repository's RustSec policy records no exception
for a vulnerability, so a version bump was not available as an alternative. The
dated evidence, including the specific versions checked and the resolver output,
is in
[`docs/archive/2026-10/rustsec-zip-extraction-2026-10.md`](../archive/2026-10/rustsec-zip-extraction-2026-10.md).

### Decision

5. **The `zip` feature is off, and turning it back on is not a migration step.**
   `src-tauri/Cargo.toml` declares
   `tauri-plugin-updater = { version = "2", default-features = false, features = ["rustls-tls", "system-proxy"] }`.
   `zip` was the only default feature dropped.

   The invariant that matters: **in the plugin, that feature is one switch
   covering two extraction paths.** It is `["dep:zip", "dep:tar", "dep:flate2"]`,
   and it gates both the Linux AppImage `.tar.gz` extraction and the Windows ZIP
   extraction. Enabling it puts the pinned `zip = "4"` back into `Cargo.lock` and
   into the Windows build, and the three advisories return with it. That holds
   regardless of which path motivated the change, and regardless of platform,
   because `Cargo.lock` is platform-independent even though the crate is not.

### Consequences

- **Adding macOS/Linux release builds (issue #835) does not mean enabling this
  feature.** AppImage `.tar.gz` extraction needs the feature, so Linux support
  requires one of: a plugin release that splits the feature or raises the `zip`
  floor to 9.x; or extracting the AppImage outside the plugin. Any of those makes
  the feature worth revisiting; "turn it on" is not a step on its own.
- **Serving a ZIP update artifact on Windows has the same requirement.** The
  reason the feature is currently safe is that no published feed key resolves to
  a ZIP artifact — the keys map to `.msi` and `.exe`, both handled by
  `extract_exe`. Publishing a `.zip` artifact would make the archive path
  reachable, so it needs the same safe extraction path or fixed dependency first.
  That feed mapping is a published-state fact and must be re-read before it is
  relied on.
- **Verified by gate, not by convention.** The RustSec advisory gate fails on any
  vulnerability with no exception mechanism, so re-enabling the feature turns the
  build red rather than silently regressing. A regression test in
  `tests/agent-infra/supply-chain-gates.test.mjs` additionally asserts `zip` stays
  out of `Cargo.lock` and out of the dependency line.

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
