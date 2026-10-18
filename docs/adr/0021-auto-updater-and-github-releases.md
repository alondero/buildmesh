# ADR 0021 — In-app auto-updater and tag-triggered GitHub Releases

## Status

Accepted (2026-07-16). Amended 2026-10-17 (issue #2438) — see [Amendment](#amendment-2026-10-17-the-updaters-zip-extraction-feature-stays-off). Amended 2026-10-18 (issue #2483) — see [Amendment](#amendment-2026-10-18-macos-joins-the-updater-feed). Decisions 1–3 are unchanged.

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

4. **Windows-only Tauri desktop installers, for now.** The app is
   Windows/WSL-centric; a cross-platform matrix would ~4x CI cost per tag and
   needs each OS to compile Windows-centric code. The `release` job runs only on
   `windows-latest`, so the desktop artifact is a Windows installer regardless of
   `bundle.targets` — the base `tauri.conf.json` sets `"targets": "all"`, and the
   release overlay does not narrow it, but a non-Windows runner is never used to
   build a tagged release. Extending the workflow to a matrix (issue #835) is also
   the right time to widen the target list deliberately rather than inherit
   `all`. Tracked as a follow-up issue.

   **Scope of "Windows-only".** That describes the *Tauri desktop installers*, not
   the release as a whole. A second job, `android-release`, builds a signed APK on
   `ubuntu-latest` and uploads it to the same GitHub Release when
   `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD` are
   configured; without them it warns and exits 0, and *that* release is genuinely
   Windows-only. The APK is uploaded as a plain asset with a `.sha256` sidecar
   and is deliberately **not** added to `latest.json`, so the auto-updater feed
   this ADR is about stays Windows-only either way. See
   [`docs/development/android.md`](../development/android.md#release-signing-and-distribution)
   and [`docs/development/releasing.md`](../development/releasing.md).
   *(Superseded in part by the 2026-10-18 amendment below: the feed now also
   carries the two macOS entries, so "Windows-only" describes the feed as of
   this decision, not as published today.)*

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

## Amendment (2026-10-18): macOS joins the updater feed

Issue #2483. This narrows decision 5's "Windows-only" feed rather than
reversing it, and it changes no decision about how the feed is signed or
served.

`latest.json` now carries `darwin-aarch64` and `darwin-x86_64` beside
`windows-x86_64`, published from a new `macos-release` job that builds one
native macOS bundle per architecture. The updater archives are `.app.tar.gz`
with a matching minisign `.sig`, signed with the **same** app-owned key as
Windows — decision 4 and the signing-keys section are unchanged, and no new
key is introduced.

What is deliberately *not* changed:

- **No Apple Developer Program membership, Developer ID certificate, Apple ID,
  team, API key, or notarization.** The macOS bundle is ad-hoc signed
  (`signingIdentity: "-"`), which needs no credential. It therefore carries no
  Developer ID trust and Gatekeeper still blocks first launch. Issue #834
  (paid signing) remains deferred and is not a prerequisite for this slice.
- **No change to how the feed is served.** `latest.json` still comes from the
  shared GitHub `latest.json` endpoint, is still written to a draft release, and
  still only becomes `releases/latest` when a human publishes that draft.
- **Android stays out of the feed**, exactly as above.

The operational consequence is that three jobs now write one feed asset.
`tauri-action` merges it per `{os}-{arch}` key, but merges by
delete-then-upload, so the workflow serialises the writers (`needs:` plus
`max-parallel: 1`) and adds a `Verify updater feed` job that reads the merged
feed back and fails unless every platform entry is present, signed and
resolvable. Packaging evidence is not installed-app runtime evidence: real
Mac launch and a real in-app update are issue #2484's gate before any public
support claim.

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
