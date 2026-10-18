---
name: release
description: Cut and publish a Buildmesh release (version bump, release notes, tag, Windows and macOS installers plus signed Android APK, post-release bump). Use when asked to release, cut, tag or publish a version such as v1.4.1.
---

# Cut a Buildmesh release

Owner doc: `docs/development/releasing.md` (versioning scheme, required checks, rationale, macOS packaging). This skill is the runnable sequence; the doc wins if they disagree.

Publishing flips the auto-updater feed for every Windows and macOS install. Only publish when the user asked for the release; stop at the draft if unsure.

## 0. Preconditions

- `gh secret list --repo alondero/buildmesh` shows `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`. A missing `ANDROID_*` secret makes the Android job warn and publish without the APK.
- The Android keystore and its password live in `~/.buildmesh-signing/` (`%USERPROFILE%\.buildmesh-signing\` on Windows). Never regenerate it (Android refuses updates signed by a different key) and never print the password.
- **No Apple credentials are needed or wanted.** The macOS job ad-hoc signs (`signingIdentity: "-"`), so there is no Developer ID certificate, Apple ID, team or API key to set up, and no annual fee. It buys no Gatekeeper trust, so macOS users still get the first-launch override — that is expected, not a broken release. Do not add notarization to a release; it is issue #834 and out of scope here.
- Decide the version `X.Y.Z`. The between-release version is `-0` and one minor ahead of the last published release, so a patch release still starts from that `-0` value and `version:set` strips it.

## 1. Prep branch

Branch from `origin/main` as `release/vX.Y.Z`.

1. `npm run version:set -- X.Y.Z`, then `npm run check:versions`. Five files change.
2. `npm run release:notes`. Read the header: base should be the previous `chore(release): vX.Y.Z` commit (the post-release bump counts) and the commit count should look right.
3. `npm run release:notes -- --write` creates the note in `docs/releases/`, named `v` + the version + `.md` (the workflow requires that exact file; `--write` refuses to overwrite one, add `--force` to regenerate). Curate it in place: keep user-visible entries only and add Highlights and Upgrade notes. Use `docs/releases/v1.4.1.md` as the shape for a small release. Line 1 must stay exactly `# Buildmesh vX.Y.Z`; the release workflow greps for it and fails the tag without it.
4. Change the "next release" link in `docs/releases/README.md` to the new file.
5. `npm run check:docs` and `npm run test:docs`.
6. Commit as `chore(release): vX.Y.Z prep - version bumps, release notes`. The commit guard rejects a version change without staged docs or a `docs:` line in the message; the release note counts as staged docs, but say so in a `docs:` line anyway. Push, then open the PR:
   - Write the body to `.tmp/pr-body.md` (git-ignored) following the sections of `.github/PULL_REQUEST_TEMPLATE.md`: Change, Evidence (a table of what actually ran), Review notes, Documentation impact.
   - `gh pr create --base main --title "chore(release): vX.Y.Z prep - version bumps, release notes" --body-file .tmp/pr-body.md`. Always `--body-file`: an inline `--body` mangles non-ASCII characters on Windows and expands backticks.
   - Use `Refs #N`, not `Closes #N`, for the tracking issue unless the release itself fulfils it.

## 2. Merge

Wait for checks: `gh pr checks <n> --watch` in the background. The summary counts the skipped `Alert on failure` job as a failure; it is not one. Then `gh pr merge <n> --squash --subject "<title> (#<n>)" --body "docs: ..."`. Omit `--delete-branch`: the branch is checked out in a worktree.

## 3. Tag the merge commit

```
git fetch origin main
git tag vX.Y.Z <merge-sha-on-origin-main>
git push origin vX.Y.Z
```

The tag must be reachable from `main` or the workflow rejects it. Do not move or reuse a pushed tag. Find the run with `gh run list --workflow release.yml --limit 1` and watch it with `gh run watch <id> --exit-status` in the background. `Verification` runs first, followed by `release` (the Windows draft). After that, the two `macOS ad-hoc signed` legs run **serially** while `Android signed APK` runs in parallel. The native downloaded-candidate audit follows the macOS legs. `Verify updater feed` waits for the Mac audit and may run in parallel with Android. The macOS package legs are sequential by design: all three packaging jobs write the same `latest.json`, and merging is delete-then-upload, so a longer macOS job is the cost of not shipping a half-written feed.

## 4. If the Android job fails

Read `gh run view <id> --log-failed`.

- `base64: invalid input` means a secret was stored with a trailing `\r\n` (the job now strips that, so this only appears on a tag cut before the strip landed). Windows PowerShell 5.1 appends one when you pipe a string to a native command (`$s | gh secret set`). Re-set the secrets from Git Bash with a redirect, which stores exact bytes:
  ```
  base64 -w0 < ~/.buildmesh-signing/buildmesh-release.jks | gh secret set ANDROID_KEYSTORE_BASE64 --repo alondero/buildmesh
  printf 'buildmesh' | gh secret set ANDROID_KEY_ALIAS --repo alondero/buildmesh
  gh secret set ANDROID_KEY_PASSWORD --repo alondero/buildmesh < ~/.buildmesh-signing/key-password.txt
  ```
- Then `gh run rerun <id> --failed`. It reuses the tagged commit and reads the secrets fresh, so no new tag is needed.
- To check whether the warning fired, look for `##[warning]` in the log, not the text `Android APK not published`; the script source echoes that text on every run.

## 5. Verify the draft before publishing

`gh release view vX.Y.Z --json isDraft,assets` must list all of: `Buildmesh_X.Y.Z_x64-setup.exe` (+`.sig`), `Buildmesh_X.Y.Z_x64_en-US.msi` (+`.sig`), `latest.json`, `SHA256SUMS.txt`, `Buildmesh_X.Y.Z_android.apk` and `Buildmesh_X.Y.Z_android.apk.sha256`, plus both macOS sets: `Buildmesh_X.Y.Z_aarch64.dmg`, `Buildmesh_X.Y.Z_aarch64.app.tar.gz` (+`.sig`), `Buildmesh_X.Y.Z_x64.dmg`, `Buildmesh_X.Y.Z_x64.app.tar.gz` (+`.sig`), `SHA256SUMS-macos-aarch64.txt`, `SHA256SUMS-macos-x86_64.txt`, and `MACOS-CANDIDATE-AUDIT-aarch64.json` / `MACOS-CANDIDATE-AUDIT-x86_64.json`. Tauri names Intel files with the `x64` suffix; `x86_64` remains the CPU and updater platform label. The installed-app receipt is added in step 6.

The `Verify updater feed` job must be green — it fails unless `latest.json` carries a signed, resolvable entry for `windows-x86_64`, `darwin-aarch64` and `darwin-x86_64`. If it is red, the draft is not publishable; read `gh run view <id> --log-failed` and fix before doing anything else.

Confirm the `Downloaded macOS candidate audit` job passed on both native runners. It downloads each DMG from the draft, compares the exact bytes with the release checksum, mounts the DMG, checks `Signature=adhoc`, executable architecture and `LSMinimumSystemVersion`, then uploads its JSON record to the draft. The record distinguishes those automated checks from browser quarantine, Gatekeeper and Finder checks, which require a human Mac session.

Download the APK into a scratch directory and check it with the Android SDK tools in `%LOCALAPPDATA%\Android\Sdk\build-tools\35.0.0`:

- `apksigner.bat verify --verbose --print-certs <apk>` prints `Verifies`, v2 true, one signer, certificate SHA-256 `6f98e9c75524a71094a981b9803c680a12572c85ddc105b60464ee46ce110695`. A different fingerprint means the wrong key signed it: do not publish.
- `aapt2.exe dump badging <apk>` reports `versionName='X.Y.Z'`.
- The SHA-256 of the file equals the sidecar.

## 6. macOS installed-candidate receipt is a publication prerequisite

Read [the installed-candidate procedure](../../../docs/development/releasing.md#before-publishing). The native workflow audit proves the exact uploaded DMG's checksum, ad-hoc signature, executable architecture and declared minimum OS. It does not prove browser quarantine, Gatekeeper approval, Finder launch, an authenticated CLI task, resume, or the in-app update flow.

Before publishing the draft:

1. Download both `MACOS-CANDIDATE-AUDIT-<arch>.json` files and both macOS checksum manifests from **that** draft.
2. Fill `docs/release-evidence/macos-installed-candidate.template.json` from clean native Mac user accounts. Record the browser-downloaded DMG digest, Gatekeeper/Finder result, one supported authenticated CLI task and diff, app close/reopen/session-resume result, and plain Terminal non-resumability. Do not claim live-process survival through reboot.
3. For each architecture, run the two-version updater smoke against a private HTTPS test feed using the tagged source, the same bundle identifier and updater key, and distinct versions. Verify successful install/relaunch with workspace state retained, then a bad-signature rejection that leaves the installed version unchanged. Keep the public `releases/latest` feed untouched. The procedure includes the config overlay and commands.
4. Set every receipt outcome to `PASS`, `FAIL` or `UNVERIFIED` with evidence. `FAIL` blocks publication. `UNVERIFIED` is allowed only with a reason and means that architecture/workflow remains early access and untested.
5. Validate the receipt and upload it to the draft as `MACOS-INSTALLED-RECEIPT.json`:
   ```sh
   COMMIT="$(git rev-parse "${TAG}^{commit}")"
   node scripts/ci/verify-macos-installed-receipt.mjs .tmp/macos-installed-candidate.json --tag "$TAG" --commit "$COMMIT" --release-dir .tmp/macos-release-evidence
   cp .tmp/macos-installed-candidate.json .tmp/MACOS-INSTALLED-RECEIPT.json
   gh release upload "$TAG" .tmp/MACOS-INSTALLED-RECEIPT.json --repo alondero/buildmesh --clobber
   ```
6. Set tested-support claims only for the architecture and CLI evidence that passed; one tested harness does not establish parity for the others.

Keep the Mac assets in the **draft** until the receipt validates. Do not infer installed runtime support from the build or feed jobs. If suitable native hardware is unavailable, attach the receipt with explicit unverified coverage and keep public support claims narrow; never report skipped checks as passes.

## 7. Publish and bump back

1. `gh release edit vX.Y.Z --draft=false --latest`, then `curl -sL https://github.com/alondero/buildmesh/releases/latest/download/latest.json` and confirm `"version": "X.Y.Z"` **and** that `platforms` contains `windows-x86_64`, `darwin-aarch64` and `darwin-x86_64`.
2. New branch from `origin/main`: `npm run version:set -- <next minor>.0-0` (after 1.4.1 that is `1.5.0-0`), `npm run check:versions`, commit `package-lock.json` with the rest as `chore(release): vX.Y.Z post-release version bump`. The message must carry `docs: none — version bump only` (a plain hyphen also passes the guard), because no docs are staged. Open the PR the same way as step 1.6 (body file, template sections), wait for checks, squash-merge with the same `docs:` line in `--body`.
3. Comment on the tracking issue with the release URL, the installed receipt, what was verified, and — for a release carrying Mac artifacts — the native architectures, tested CLI, update/rejection outcomes and any unverified coverage. Include the declared `LSMinimumSystemVersion`, clearly labeled as a bundle value rather than a runtime minimum when native runtime evidence is absent.

## Report to the user

Release URL, the assets present, the three APK checks, the macOS signature and minimum-version findings, and anything unverified. Report a skipped or failed gate as such, and say plainly when the Mac install/upgrade check has not been run.
