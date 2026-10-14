---
name: release
description: Cut and publish a Buildmesh release (version bump, release notes, tag, Windows installers plus signed Android APK, post-release bump). Use when asked to release, cut, tag or publish a version such as v1.4.1.
---

# Cut a Buildmesh release

Owner doc: `docs/development/releasing.md` (versioning scheme, required checks, rationale). This skill is the runnable sequence; the doc wins if they disagree.

Publishing flips the Windows auto-updater feed for every install. Only publish when the user asked for the release; stop at the draft if unsure.

## 0. Preconditions

- `gh secret list --repo alondero/buildmesh` shows `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`. A missing `ANDROID_*` secret makes the Android job warn and publish a Windows-only release.
- The Android keystore and its password live in `~/.buildmesh-signing/` (`%USERPROFILE%\.buildmesh-signing\` on Windows). Never regenerate it (Android refuses updates signed by a different key) and never print the password.
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

The tag must be reachable from `main` or the workflow rejects it. Do not move or reuse a pushed tag. Find the run with `gh run list --workflow release.yml --limit 1` and watch it with `gh run watch <id> --exit-status` in the background. Order: `Verification` (about 7 min), then `release` (Windows draft, about 17 min), then `Android signed APK` (about 3 min); roughly 30 minutes in total (v1.4.1).

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

`gh release view vX.Y.Z --json isDraft,assets` must list all of: `Buildmesh_X.Y.Z_x64-setup.exe` (+`.sig`), `Buildmesh_X.Y.Z_x64_en-US.msi` (+`.sig`), `latest.json`, `SHA256SUMS.txt`, `Buildmesh_X.Y.Z_android.apk` and `Buildmesh_X.Y.Z_android.apk.sha256`.

Download the APK into a scratch directory and check it with the Android SDK tools in `%LOCALAPPDATA%\Android\Sdk\build-tools\35.0.0`:

- `apksigner.bat verify --verbose --print-certs <apk>` prints `Verifies`, v2 true, one signer, certificate SHA-256 `6f98e9c75524a71094a981b9803c680a12572c85ddc105b60464ee46ce110695`. A different fingerprint means the wrong key signed it: do not publish.
- `aapt2.exe dump badging <apk>` reports `versionName='X.Y.Z'`.
- The SHA-256 of the file equals the sidecar.

## 6. Publish and bump back

1. `gh release edit vX.Y.Z --draft=false --latest`, then `curl -sL https://github.com/alondero/buildmesh/releases/latest/download/latest.json` and confirm `"version": "X.Y.Z"`.
2. New branch from `origin/main`: `npm run version:set -- <next minor>.0-0` (after 1.4.1 that is `1.5.0-0`), `npm run check:versions`, commit `package-lock.json` with the rest as `chore(release): vX.Y.Z post-release version bump`. The message must carry `docs: none — version bump only` (a plain hyphen also passes the guard), because no docs are staged. Open the PR the same way as step 1.6 (body file, template sections), wait for checks, squash-merge with the same `docs:` line in `--body`.
3. Comment on the tracking issue with the release URL and what was verified.

## Report to the user

Release URL, the assets present, the three APK checks, and anything unverified. Report a skipped or failed gate as such.
