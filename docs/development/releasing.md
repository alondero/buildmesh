# Releasing Buildmesh

Status: current

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

1. **Strip the suffix and set the release version in every file that stores it**
   (they must agree exactly with the git tag — the release workflow enforces
   string equality):
   ```
   npm run version:set -- 1.2.0
   ```
   This updates `package.json`, `src-tauri/tauri.conf.json`,
   `src-tauri/Cargo.toml`, the `buildmesh` entry in `src-tauri/Cargo.lock`, and
   `package-lock.json` — commit all five. `package-lock.json` is easy to
   forget because it looks like build output rather than a manifest, but npm
   stores the root version in it twice (the top-level mirror and the
   `packages[""]` entry) and rewrites both on the next install.
2. Draft the release note:
   ```
   npm run release:notes
   ```
   This prints a draft to stdout from the Conventional Commits merged since
   the previous release — the version defaults to the manifest, the base to the
   `chore(release): vX.Y.Z` commit for the highest already-released version
   below this one. Read the header: it states the resolved base and commit
   count, so a wrong range is visible before you curate anything.

   To write the file instead of printing it:
   ```
   npm run release:notes -- --write
   ```
   This writes `docs/releases/v1.2.0.md`. It **fails if that file already
   exists** — which is the normal case once a draft is open. If the draft is
   already there, curate it in place; to regenerate over it, add `--force`.

   Either way, curate before tagging: keep the user-visible entries, drop the
   internal work, and add the Highlights and Upgrade notes a reader needs. The
   release workflow checks that this exact file exists and uses it as the GitHub
   Release body.
3. Commit the version bump and the generated release note, then merge to `main`.
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
   Commit `package-lock.json` with the rest: a bump that leaves the lockfile on
   the released version is the exact drift the `Manifest versions` job and the
   release tag gate now reject.

One version, five files. `npm run version:set` is the fanout that keeps them
in step, and `npm run check:versions`
(`scripts/check-manifest-versions.mjs`) is the read-only check that all six
version sites still agree. Run it after any hand edit to a version, and let
CI run it for you: the `Manifest versions` job in
`.github/workflows/verify.yml` reads the same script on every pull request, and
`release.yml` runs it with the tag as the expected version. It needs no
`npm ci` and no build, so it is the cheapest gate in the graph.

### Where the version check does and does not block a merge

The `Manifest versions` job is **not** in the required-check ruleset, so
understand what still stands behind it:

- A bump made with `npm run version:set` always writes `package.json`, which
  change-scope classifies as a frontend change, so the **required**
  `Quality (Linux)` job runs the vitest suite and its version assertion fails
  the pull request. This is the normal path and it is merge-blocking.
- A hand edit that touches **only** Rust-side manifests (`src-tauri/Cargo.toml`,
  `src-tauri/Cargo.lock`, `src-tauri/tauri.conf.json`) classifies as `rust`, not
  `frontend`. The vitest suite is then skipped, and nothing in the Rust graph
  compares a version, so that pull request merges unless a human reads the red
  `Manifest versions` job.
- A release can never ship drift either way: the tag gate in `release.yml`
  compares all five files against the tag before anything is built.

Closing the middle case means promoting `Manifest versions` to a required
check — one ruleset edit, with the command in
[Required checks and branch protection](#required-checks-and-branch-protection).
Until then, read that job's result on any pull request that edits a version.

The workspace also holds `src-tauri/proc-macros/Cargo.toml`
(`buildmesh_macros`), which is deliberately *not* one of the five: it is an
unpublished path dependency with its own version line, and it is not part of
the shipped app version. `npm run version:set` does not touch it, and
`check:versions` does not read it.

Versioning is manual/ad-hoc for now (no fixed cadence). Use semver.

Release notes are versioned under [`docs/releases/`](../releases/) and drafted
from the merged Conventional Commits — `npm run release:notes` — rather than
edited by each pull request. Include features, fixes, security changes, breaking
changes, migrations, and known limitations; curate out internal implementation
work.

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
| `Verification / Manifest versions` | no — see the gap below | Every file that stores the app version agrees on it: `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, the `buildmesh` entry in `src-tauri/Cargo.lock`, and both version sites in `package-lock.json`. A pure read of six strings, so it runs on every event with no `npm ci`, no change-scope classification, and no upstream job. |
| `Verification / Supply chain` | no — blocks a release, not a merge | Every third-party `uses:` is pinned to a full commit SHA (each SHA resolved against the GitHub API), `npm audit` passes the two-tier advisory policy for both production and dev dependencies, and `cargo audit` finds no vulnerability with every maintenance warning recorded with an owner, rationale and review date. Runs on every event rather than behind the change-scope fan-out, because an unpinned action or a lockfile bump touches no Rust source file — a gate behind that classification would miss exactly the diff it exists to catch. See [supply-chain.md](supply-chain.md) for the policy and for why it is not yet required. |
| `Verification / Quality (Linux)` | yes | The aggregate frontend gate. It runs no check of its own, and it never skips: a required check that GitHub reports as *skipped* counts as satisfied, so the decision lives in `scripts/ci/quality-gate.mjs` where the reason an upstream branch is absent is visible. It passes only when the change-scope job succeeded, `Quality gates (Linux)` passed (agent-infrastructure, docs, README-drift, ESLint (+ fixture verifier), frontend build, bundle budget, process-spawn discipline — this is what stops a docs-only pull request), and the three `Quality vitest (<leg>)` legs passed. The one absence it tolerates: the vitest legs skipping themselves when the classification says no frontend changed. |
| `Verification / Rust tests + TS bindings` | yes | The aggregate Rust gate. It passes only when the change-scope job succeeded and the compile job, every test shard, `Quality (Linux)`, and the non-shard `Rust export, doc, and integration tests` job (export, doctest, and integration targets run serially, with ts-rs regenerating `src/types/generated/` so binding drift fails the build) all passed. The one check that legitimately skips: a pull request whose diff touched no Rust (a skipped required check counts as satisfied, which is why every other absence is made to fail instead). |
| `Verification / Verify-smoke (Linux)` | yes | The real browser renders the app with a mock backend (`verify-smoke` Playwright project), whenever the change-scope job reports frontend changes; a Rust-only pull request skips it. |
| `Verification / Platform smoke (windows-latest)` | no — post-merge signal | The Tauri app compiles and links on Windows; ConPTY frame ordering and background inference behavior tests pass, including Claude install fallbacks with a stale PATH. Runs on pushes to `main`, release tags, and manual dispatches — not on pull requests. Caches its Cargo target directory; macOS deliberately does not. |
| `Verification / Platform smoke (macos-latest)` | no — post-merge signal | The Tauri app compiles and links on macOS. Same triggers as the Windows leg, cold apart from the Cargo download cache. |

The jobs fan out rather than chain. `Detect changes` classifies the pull
request's diff (everything else — pushes, the schedule, dispatches, release
tags — is classified as full scope), and the frontend branch — the **required**
`Quality (Linux)` aggregate over `Quality gates (Linux)` and three
`Quality vitest (<leg>)` legs — starts immediately after it. The Rust branch —
the **non-required** `Rust build (compile)` job, the seven `Rust tests (<group>)`
shards, and the required `Rust tests + TS bindings` aggregate — only starts when
that classification says Rust moved, and the vitest legs and `Verify-smoke
(Linux)` only run when it says frontend moved, so a Rust-only pull request never
boots Chromium and a frontend-only pull request never compiles Rust. The two
`Platform smoke` jobs are outside all of this: they are not required checks (see
the table above) and run on pushes, release tags, and manual dispatches rather
than on pull requests. The shards and the non-shard `Rust export, doc, and
integration tests` job start together once `Rust build (compile)` has populated
the shared Cargo cache; the non-shard pass is the longest Rust leg, so it no
longer waits behind the slowest shard. The `Rust tests + TS bindings` aggregate
then only checks that `Quality (Linux)`, `Rust build (compile)`, every shard, the
non-shard pass, and `Detect changes` all succeeded, so it certifies a fully
green tree without adding a serial stage of its own. The old shape queued
everything behind one ~10-minute frontend job; this one does not. `Rust build
(compile)` compiles every Rust test binary once — with `lld` and a runner
swapfile in place of the old single-threaded `CARGO_BUILD_JOBS=1` — and the
shards restore that cache instead of rebuilding.

`Quality (Linux)` keeps its name and does no work: the static gates and the
vitest suites run as parallel jobs, because one `vitest run tests/unit
tests/integration` measured 220s inside a ~310s job and was the whole critical
path for a frontend-only pull request. The unit suite is split by vitest's own
`--shard` (which gives every file to exactly one shard) and the ten-file
integration suite — 88s of serial test time, 80s of it the browser-bound
`ui-shot.test.ts` — stays whole in a third leg. That third leg is also the only
one that installs Chromium, because `ui-shot.test.ts` is the only test that
calls `launchChromium`; the unit files that mention Playwright read a
`ui-shot-*.steps.mjs` file as text or quote Chromium in a comment.
`tests/agent-infra/vitest-legs.test.mjs`
(`npm run test:agent`) gates that matrix: the shard indices must be 1..N over a
single N and the suite directories must cover exactly the two the single
combined invocation named, so a renumbered shard or an unclaimed suite directory
fails the build instead of silently skipping tests.

Two required checks — `Quality (Linux)` and `Rust tests + TS bindings` — are
aggregates that keep `if: always()` and an explicit result check rather than
relying on the implicit skip, because a required status check GitHub reports as
*skipped* counts as satisfied. Each fails with a reason instead, including when
the change-scope job itself failed. `Quality (Linux)` must not carry a skip
condition of its own, for a reason worth stating because it is easy to
reintroduce: `Rust tests + TS bindings` requires `Quality (Linux)` to equal
`success`, so a skip on one class turns that required check red, and any skip
also leaves a docs-only pull request mergeable over a red `Quality gates
(Linux)`. An earlier revision of this split skipped on
`needs.changes.outputs.frontend == 'false'` and broke both. The rules live in
`scripts/ci/quality-gate.mjs` and are tested per classification class in
`tests/agent-infra/quality-gate.test.mjs`; the wall-clock win came from running
`Quality gates (Linux)` and the vitest legs in parallel, not from skipping.

A caller-supplied `profile` input narrows the graph for pushes to `main`:
`build.yml` passes `light`, which skips the Rust branch entirely (the merge
gate already ran it against the same commit through the pull request), while
pull requests, the weekly schedule, manual dispatches, and release tags get
`full`. Required checks are only ever evaluated on pull-request runs, so the
narrowing never weakens the gate.

The Rust unit target also runs as seven parallel `Rust tests (<group>)` jobs —
`db`, `services`, `agent`, `commands-http`, `circuit-coordinator`,
`git-env-preferences`, `remaining`. They exist because a hosted runner lost
mid-`cargo-test` reports no step conclusion and no log, so a single combined
run cannot say which test did it; one job per group means a loss costs one
group. The shards are **not** required checks — `Rust tests + TS bindings` is
the gate, and the non-shard job is the single writer of the generated bindings.

Each Rust test step runs under `scripts/ci/run-guarded.mjs` (10 minutes for a
shard, 45 for the non-shard pass) beneath a longer job cap (15 and 60
minutes), and writes its log to a file that an `if: always()` upload step
preserves. The guard streams the command's output into the step log while
appending it to that file, kills the command's whole process tree at the
deadline (SIGTERM, then SIGKILL after a grace period), emits the `::error::`
annotation itself, and exits with the
command's own code — or 124 when the deadline fired. It is unit-tested in
`tests/agent-infra/run-guarded.test.mjs`, which pins the exit-code contract,
the tree kill, and the annotation.

The guard exists because of #1961: with a `| tee` pipeline, a descendant that
leaves the process group still holds the pipe open, so `tee` never sees EOF and
the step outlives the guard that was supposed to end it — run 36531715263 lost
the `services` shard that way, about 45 minutes with no step conclusion and no
log, because a cancelled job flushes neither. The guard never waits
*unboundedly* on the output pipes after the kill: every wait path has its own
cap, so a descendant holding the write end cannot keep the step alive past its
deadline, and because output reaches the log file as it arrives rather than at
the end, even a hard job-level cancel leaves a retrievable log. A hung test
therefore costs one failed shard with evidence, instead of an open-ended stall
that holds the required `Rust tests + TS bindings` gate open.

Because libtest filters are substring matches, they cannot express "this
test's first path segment is X", so the split is a list of exact filters and
`--skip`s. A new top-level module would therefore go unrun silently unless it
is claimed, which `npm run check:rust-shards`
(`scripts/check-rust-shard-coverage.mjs`, also a CI step) prevents: it lists
the unit tests from the test binary and fails if any is unclaimed or claimed
twice. Run it after adding a module, not only in CI.

Seven shards were re-measured after #2048 and deliberately kept: the seven
shards now run 13-39s of tests each, so per-shard setup is back on the critical
path instead of the tests, and consolidating into fewer, larger shards would
*lengthen* it (~40s + 173/k) rather than shorten it. Revisit if the slowest
shard grows past the setup cost again.

### Caching, and the 10 GB budget

GitHub caps a repository's caches at 10 GB in total and evicts the
least-recently-used entries past that. This repository sat at 9.98 GB before the
following changes, so **a new cache can silently evict the Cargo target cache
that turns `rust-build`'s compile into 54 seconds instead of minutes.** Two
changes keep the budget sustainable:

- The Linux Rust jobs share one rust-cache entry (`shared-key:
  linux-rust-target`) instead of one per job id. They were saving nine
  near-identical ~590 MB copies of the same dependency artifacts — rust-cache
  never stores the workspace crate, so the duplication bought nothing.
- The apt `.deb` set (`.github/actions/install-linux-build-deps`) and the
  Playwright browser build are cached. apt keeps them in
  `Dir::Cache::archives`, which the composite action redirects into the
  workspace, so this uses apt's own supported mechanism: on a hit apt still
  resolves and verifies every package and runs its maintainer scripts, and only
  the transfer is skipped. A stale or partial entry costs a download, not a
  broken job.

Check the budget before adding another cache:

```
gh api "repos/alondero/buildmesh/actions/caches?per_page=100"
```

Two caches are deliberately *not* kept: the macOS platform smoke's target
directory (the cache action documents macOS target directories as its
corruption workaround, and that leg is 245s) and the weekly `Weekly package
smoke` targets (a one-off check does not justify three persistent target
caches).

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

Infra flakiness clears itself, too: whenever a Build run ends unsuccessfully,
and every 15 minutes as a backstop, `.github/workflows/ci-retry.yml` runs
`scripts/ci/retry-failed-runs.mjs`, which re-runs the failed jobs of at most
three Build runs — first attempts only, less than six hours
old, the newest run for their event and head branch, and for pull requests
only while the PR is still open. It never retries a second time, so a
genuinely broken tree stays red instead of being re-rolled until it goes
green, and `--failed` means a blip in one shard re-runs that shard rather than
the whole fan-out. A lost runner reports nothing until its job cap expires, so
the Rust shards cap at 15 minutes: that cap, not the retry, is what a lost
shard runner costs.

`WSL Codex profile contract (opt-in)` is `workflow_dispatch`-only. It runs a
`#[ignore]`d test that needs a real WSL guest, and a hosted Windows image ships
the feature without a distribution, so the job imports one itself. Without
that step the job is red for want of a guest rather than for a contract
breakage, and — because `Alert on failure` watches this job too — it opens a
`ci-alert` issue describing a failure the weekly package smoke did not have.

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
2. Tag/version agreement — all five version-bearing files must match the tag:
   `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, the
   `buildmesh` entry in `src-tauri/Cargo.lock`, and `package-lock.json`. The
   step runs `scripts/check-manifest-versions.mjs --expect <tag>`, the same
   check the `Manifest versions` job runs on every pull request, so a release
   cannot be cut from a tree the merge gate would have rejected.
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
