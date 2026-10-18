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

The tag must be reachable from `main` or the workflow rejects it. Do not move or reuse a pushed tag. Find the run with `gh run list --workflow release.yml --limit 1` and watch it with `gh run watch <id> --exit-status` in the background. Job order: `Verification` (about 7 min), then `release` (Windows draft, about 17 min), then `macOS ad-hoc signed` (two legs, **serialised** — one build each, so budget roughly double a single leg), `Android signed APK` (about 3 min), then `Verify updater feed`. The macOS legs are sequential by design: all three packaging jobs write the same `latest.json`, and merging is delete-then-upload, so a longer macOS job is the cost of not shipping a half-written feed.

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

`gh release view vX.Y.Z --json isDraft,assets` must list all of: `Buildmesh_X.Y.Z_x64-setup.exe` (+`.sig`), `Buildmesh_X.Y.Z_x64_en-US.msi` (+`.sig`), `latest.json`, `SHA256SUMS.txt`, `Buildmesh_X.Y.Z_android.apk` and `Buildmesh_X.Y.Z_android.apk.sha256`, plus both macOS sets: `Buildmesh_X.Y.Z_aarch64.dmg`, `Buildmesh_X.Y.Z_aarch64.app.tar.gz` (+`.sig`), `Buildmesh_X.Y.Z_x86_64.dmg`, `Buildmesh_X.Y.Z_x86_64.app.tar.gz` (+`.sig`), `SHA256SUMS-macos-aarch64.txt` and `SHA256SUMS-macos-x86_64.txt`.

The `Verify updater feed` job must be green — it fails unless `latest.json` carries a signed, resolvable entry for `windows-x86_64`, `darwin-aarch64` and `darwin-x86_64`. If it is red, the draft is not publishable; read `gh run view <id> --log-failed` and fix before doing anything else.

Confirm the macOS job actually signed ad-hoc rather than trusting the green run: `gh run view <id> --log` on the `macOS ad-hoc signed` job shows `Signature=adhoc` from `codesign -dv` and the `LSMinimumSystemVersion` each bundle declared. Record those two numbers — they are the evidence for what the release supports, not an assumption.

Download the APK into a scratch directory and check it with the Android SDK tools in `%LOCALAPPDATA%\Android\Sdk\build-tools\35.0.0`:

- `apksigner.bat verify --verbose --print-certs <apk>` prints `Verifies`, v2 true, one signer, certificate SHA-256 `6f98e9c75524a71094a981b9803c680a12572c85ddc105b60464ee46ce110695`. A different fingerprint means the wrong key signed it: do not publish.
- `aapt2.exe dump badging <apk>` reports `versionName='X.Y.Z'`.
- The SHA-256 of the file equals the sidecar.

## 6. macOS install validation is a publication prerequisite

Before publishing a draft that carries Mac artifacts, the Mac download-and-run check must be done against **that** candidate: download the DMG, mount it, confirm the bundle's `codesign --verify --deep --strict` passes, launch it, and run one real update through the in-app updater. Record the result in the tracking issue.

Packaging evidence is not installed-app runtime evidence, and this is the split that keeps the two honest. The release workflow proves the bundle was built and the feed is well-formed; it cannot prove a Mac launches the app or that the updater replaces it. Keep the Mac assets in the **draft** until that check is done — do not publish, and do not treat "the macOS job was green" as a substitute. Do not claim a minimum macOS version, a working Finder launch or a working update path on the strength of the CI logs alone.

## 7. Publish and bump back

1. `gh release edit vX.Y.Z --draft=false --latest`, then `curl -sL https://github.com/alondero/buildmesh/releases/latest/download/latest.json` and confirm `"version": "X.Y.Z"` **and** that `platforms` contains `windows-x86_64`, `darwin-aarch64` and `darwin-x86_64`.
2. New branch from `origin/main`: `npm run version:set -- <next minor>.0-0` (after 1.4.1 that is `1.5.0-0`), `npm run check:versions`, commit `package-lock.json` with the rest as `chore(release): vX.Y.Z post-release version bump`. The message must carry `docs: none — version bump only` (a plain hyphen also passes the guard), because no docs are staged. Open the PR the same way as step 1.6 (body file, template sections), wait for checks, squash-merge with the same `docs:` line in `--body`.
3. Comment on the tracking issue with the release URL, what was verified, and — for a release carrying Mac artifacts — the macOS install/upgrade result and the `LSMinimumSystemVersion` observed.

## Report to the user

Release URL, the assets present, the three APK checks, the macOS signature and minimum-version findings, and anything unverified. Report a skipped or failed gate as such, and say plainly when the Mac install/upgrade check has not been run.
