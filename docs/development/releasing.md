# Releasing Buildmesh

Buildmesh ships an in-app auto-updater (issue #826, ADR 0021). This is how to
cut a release and the one-time setup behind it.

## Cutting a release

### Versioning scheme: `-0` between releases

The version in the manifests carries a `-0` suffix between releases, always
one minor ahead of the last published release (e.g. after publishing v1.2.0,
local builds are `1.3.0-0`). The identifier is numeric because Windows MSI
(WiX) rejects non-numeric prereleases (`1.3.0-dev` fails bundling with
"optional pre-release identifier in app version must be numeric-only"). It
does not increment: every between-release build stays at `-0` until the next
tagged release.

This matters for the auto-updater: it compares versions with semver, so a
locally built `1.3.0-0` is *newer* than the published `1.2.0` and the app will
**not** show an "Update available" prompt for your own local builds. Without
this, any production-profile build you make locally nags you to "update" to
the release you already have.

(Dev-profile builds — `npm run tauri:build:dev` — disable the updater entirely
via their `.dev` bundle identifier; this scheme covers plain `tauri build`
output.)

### Steps

1. **Strip the suffix and set the release version in all three manifests** (they
   must agree exactly with the git tag — the release workflow enforces string
   equality):
   ```
   npm run version:set -- 1.2.0
   ```
   This updates `package.json`, `src-tauri/tauri.conf.json`,
   `src-tauri/Cargo.toml`, and the `buildmesh` entry in `src-tauri/Cargo.lock`.
2. Commit the bump and merge to `main`.
3. **Push a matching tag** — this is the only trigger for the release build:
   ```
   git tag v1.2.0
   git push origin v1.2.0
   ```
4. The `Release` workflow (`.github/workflows/release.yml`) builds the Windows
   installer + updater artifacts, signs them, and creates a **draft** GitHub
   Release containing the installer, its `.sig`, and `latest.json`.
5. Review the draft release on GitHub and **publish** it. Once published,
   `…/releases/latest/download/latest.json` serves the feed, and running installs
   will show the "Update available" prompt on next launch.
6. **Immediately bump back to the next `-0` version**:
   ```
   npm run version:set -- 1.3.0-0
   ```
   Commit and merge so subsequent local builds stay newer than the release.

Versioning is manual/ad-hoc for now (no fixed cadence). Use semver.

## One-time setup: updater signing secrets

The release build signs each update package with a minisign private key so the
app can verify it (via the public key committed in `tauri.conf.json`). This is
**not** OS code signing — see below.

The keypair was generated once with `tauri signer generate` and lives at
`~/.tauri/buildmesh.key` (private) / `~/.tauri/buildmesh.key.pub` (public, already
committed). Load the private key into the repo's GitHub Actions secrets:

```
gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/buildmesh.key
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --body ""
```

(The key was generated with an empty password, hence the empty body. If you
regenerate with a password, set it here.) Keep the private key file backed up
somewhere safe and **never commit it** — `src-tauri/.gitignore` blocks `*.key`
as a net.

## SmartScreen / Gatekeeper warnings (unsigned installers)

Buildmesh installers are **not** OS-code-signed, so first launch shows:

- **Windows** — SmartScreen: *"Windows protected your PC … unknown publisher."*
  Click **More info → Run anyway**.
- **macOS** — Gatekeeper: *"cannot be opened because the developer cannot be
  verified."* Right-click the app → **Open**, then **Open** again; or
  System Settings → Privacy & Security → **Open Anyway**.

This is expected for an internal tool. OS code signing (Authenticode /
Apple notarization) would remove these warnings but requires paid certificates —
it is deferred (tracked in issue #834). The auto-updater is unaffected by
this: it verifies updates with its own minisign signature regardless.
