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
2. Create or update `docs/releases/v1.2.0.md` with the concise, user-visible
   changes for this release. The release workflow checks that this exact file
   exists and uses it as the GitHub Release body.
3. Commit the version bump and release notes, then merge to `main`.
4. **Push a matching tag** — this is the only trigger for the release build:
   ```
   git tag v1.2.0
   git push origin v1.2.0
   ```
5. The `Release` workflow (`.github/workflows/release.yml`) first re-runs the
   full verification set on the tagged commit, then builds the Windows
   installer + updater artifacts, signs them, and creates a **draft** GitHub
   Release containing the installer, its `.sig`, and `latest.json`. If
   verification fails on the tag, nothing is built and no release exists —
   see [What blocks a release](#what-blocks-a-release).
6. Review the draft release on GitHub and **publish** it. Once published,
   `…/releases/latest/download/latest.json` serves the feed, and running installs
   will show the "Update available" prompt on next launch.
7. Point the [release-notes guide](../releases/README.md) at the next draft
   when one is ready. Do not rewrite the published note to describe later work.
8. **Immediately bump back to the next `-0` version**:
   ```
   npm run version:set -- 1.3.0-0
   ```
   Commit and merge so subsequent local builds stay newer than the release.

Versioning is manual/ad-hoc for now (no fixed cadence). Use semver.

Release notes are versioned under [`docs/releases/`](../releases/). Include
features, fixes, security changes, breaking changes, migrations, and known
limitations; do not add internal implementation work or rely on a generic
workflow-generated body.

## Required checks and branch protection

`main` is protected in two places, because the two mechanisms do not cover the
same ground:

- The `main: verified merges only` **ruleset** requires a pull request (no
  direct pushes), blocks deletion and force-push, and requires the checks below
  to pass on the head commit with the branch up to date. It has **no bypass
  actors**, so administrators are subject to it too.
- **Classic branch protection** carries `required_conversation_resolution`, with
  `enforce_admins` on. The rulesets API on this repository rejects
  `required_conversation_resolution` in every form (bare, empty parameters,
  null), so conversation resolution is enforced there instead of in the
  ruleset. That split is deliberate, not an oversight.

| Check | Required | What it proves |
|---|---|---|
| `Verification / Quality (Linux)` | yes | Agent-infrastructure, docs, README-drift, ESLint (+ fixture verifier), frontend build, bundle budget, vitest unit + integration, and a compile of every Rust test target. |
| `Verification / Rust tests + TS bindings` | yes | The full Rust suite, run serially, with ts-rs regenerating `src/types/generated/` so binding drift fails the build. It also refuses to run unless every test shard below passed. |
| `Verification / Verify-smoke (Linux)` | yes | The real browser renders the app with a mock backend (`verify-smoke` Playwright project). |
| `Verification / Platform smoke (windows-latest)` | yes | The Tauri app compiles and links on Windows; ConPTY frame ordering holds. |
| `Verification / Platform smoke (macos-latest)` | yes | The Tauri app compiles and links on macOS. |

The Rust unit target also runs as seven parallel `Rust tests (<group>)` jobs —
`db`, `services`, `agent`, `commands-http`, `autopilot-coordinator`,
`git-env-preferences`, `remaining`. They exist because a hosted runner lost
mid-`cargo-test` reports no step conclusion and no log, so a single combined
run cannot say which test did it; one job per group means a loss costs one
group. The shards are **not** required checks — `Rust tests + TS bindings` is
the authoritative, single-writer pass and the gate.

Because libtest filters are substring matches, they cannot express "this
test's first path segment is X", so the split is a list of exact filters and
`--skip`s. A new top-level module would therefore go unrun silently unless it
is claimed, which `npm run check:rust-shards`
(`scripts/check-rust-shard-coverage.mjs`, also a CI step) prevents: it lists
the unit tests from the test binary and fails if any is unclaimed or claimed
twice. Run it after adding a module, not only in CI.

Those names are owned by `.github/workflows/verify.yml`. A job that calls a
reusable workflow is reported as `<calling job> / <called job>`, so the
`Verification / …` prefix comes from the `verify` job in `build.yml` — its
`name:` is `Verification`, and the job id (`verify`) does not appear. Both the
`name:` in the caller and the job names in the callee are part of the
required-check identity, so **changing either is a branch-protection change**:
update the ruleset and this table in the same commit, or every pull request
will block on a check that no longer exists. The same applies to the matrix
`os:` values, which are part of the platform-smoke check names.

To read or change the required set:

```
gh api repos/alondero/buildmesh/rulesets
gh api -X PUT repos/alondero/buildmesh/rulesets/<id> --input ruleset.json
```

A weekly schedule additionally runs `Weekly package smoke` on all three
platforms. It is not merge-gating: a weekly packaging failure is reported by
opening or updating a `ci-alert` issue from the workflow itself, because a
scheduled run has no pull request to turn red. Its concurrency group is keyed
by event name, so a push to `main` cannot cancel a weekly run and suppress the
alert it would have raised.

GitHub disables scheduled workflows after 60 days without repository activity.
If the weekly packaging stops appearing, check the workflow is still `active`
(re-enable it under **Actions → Build → … → Enable workflow**) rather than
assuming the packages are fine.

## What blocks a release

`release.yml` never builds or publishes from an unverified commit:

1. `needs: verify` — the same three required jobs run against the tagged SHA
   first. `tauri-action` is downstream of that job, so a failing typecheck,
   test, lint, docs, or platform compile produces no draft release and no
   uploaded installer.
2. Tag/version agreement — `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`,
   and `package.json` must all match the tag.
3. Mainline — the tagged commit must be reachable from `main`. A tag cut from a
   side branch, or from a commit that never went through the ruleset, fails
   before the build.

Verifying on the tag rather than trusting the earlier push-to-main run is
deliberate: it is the tag's own SHA that becomes the shipped artifact. The
tag-specific steps (pinned-Codex contract smoke, WSL profile contract) are
`workflow_dispatch`-only and do not run for a release.

Packaging the installer and launching the installed application is a separate
gate tracked in issue #1522; until that lands, a release proves the tree is
sound and the bundle is produced, not that a clean install boots.

## Emergency bypass procedure

Required checks can be bypassed, but only deliberately and only on the record.
Use this when CI itself is broken and a fix cannot wait for a green run (a
runner or Actions outage, a dependency registry failure, or a false failure
that would otherwise block a security fix). Prefer fixing CI over bypassing it.

1. Say why in the pull request, and get a second opinion — the review
   conversation is the audit trail and is also required to be resolved.
2. Local checks still apply: run the smallest relevant `scripts\check.ps1`
   target plus `npm run check:docs` and record the output in the PR. A bypass is
   not a substitute for evidence, only for a missing remote run.
3. Set the ruleset to `disabled` with an empty rule set, rather than deleting
   it, so the intended policy stays visible while the exception is in force.
   A ruleset `PUT` replaces the whole document, so send a complete one:
   ```
   gh api repos/alondero/buildmesh/rulesets --jq '.[] | select(.name=="main: verified merges only") | .id'
   # body: name, target: branch, enforcement: disabled, bypass_actors: [],
   #       conditions { ref_name { include: ["~DEFAULT_BRANCH"] } }, rules: []
   gh api -X PUT repos/alondero/buildmesh/rulesets/<ruleset-id> --input ruleset-disabled.json
   ```
   Emptying the rules lifts the pull-request requirement too, so the branch can
   be merged directly. Conversation resolution lives in classic branch
   protection, not in this ruleset — leave it on unless the exception is
   specifically about unresolved threads.
4. Merge, then restore the ruleset in the same day. The bypass is a
   time-boxed exception, not a new default:
   ```
   gh api -X PUT repos/alondero/buildmesh/rulesets/<ruleset-id> --input ruleset.json
   gh api repos/alondero/buildmesh/rulesets/<id> --jq .enforcement   # must read active
   ```
   The `ruleset.json` body is the current ruleset definition
   (`gh api repos/alondero/buildmesh/rulesets/<id>`, minus `id`, `node_id`, and
   `created_at`/`updated_at`). This disable/restore round trip has been
   executed against the live repository, so the commands above are known to
   work rather than aspirational.
5. Never bypass the release gate to ship a hotfix. If a tagged commit cannot
   pass verification, the fix is a new commit on a branch that can, then a new
   tag.

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

## Verify a published installer

The `.sig` asset verifies update integrity; it is separate from OS code signing
and does not remove SmartScreen or Gatekeeper warnings. Download the installer
and matching `.sig` from the same GitHub release, then verify it with
[minisign](https://jedisct1.github.io/minisign/). The updater public-key
fingerprint is `B754F88FD69AD0DD` and the public key is committed in
`src-tauri/tauri.conf.json`.

PowerShell example:

```powershell
$publicKey = 'RWTd0JrWj/hUt82KDADoxTpTfqs8p6/ay6iht36EPXl2feP892wH1aBG'
$installer = Get-ChildItem .\Buildmesh_*_x64-setup.exe | Select-Object -First 1
minisign -Vm $installer.FullName `
  -x "$($installer.FullName).sig" `
  -P $publicKey
Get-FileHash $installer.FullName -Algorithm SHA256
Get-Content .\SHA256SUMS.txt
```

The command must report a valid signature before installation. Compare the
SHA-256 output with the matching filename in the published `SHA256SUMS.txt`.
Never replace the public key with a key copied from an untrusted release
comment. The checksum file is a second download-integrity check; it does not
replace the cryptographic signature.
