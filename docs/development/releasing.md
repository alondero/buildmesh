# Releasing Buildmesh

Status: current

Buildmesh ships an in-app auto-updater (issue #826, ADR 0021). This is how to
cut a release and the one-time setup behind it.

## Cutting a release

The `/release` skill (`.claude/skills/release/SKILL.md`) runs the sequence below
end to end, including verifying the Android APK before the draft is published.

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

   Leave the file ending with a newline. The release workflow loads a note with or
   without one — it emits its own terminating newline before the closing
   `$GITHUB_OUTPUT` delimiter — so this is a repository invariant rather than a
   release-day hazard. `npm run check:docs` fails a versioned note whose last
   byte is not a line break, which surfaces the mistake in review instead of at
   the last step of a release.

   A long gap between releases is expected on an active branch and is not a
   gate: `tests/agent-infra/release-notes.test.mjs` derives its ceiling from the
   ranges this repository has actually shipped, or from the reachability count
   until there are two release boundaries to measure, and warns through the test
   output once the range passes three quarters of the ceiling. It used to assert
   a fixed `< 200` commits, which counted down the whole repository and failed
   pull requests for their own size rather than for a wrong range (issue
   #2168). The base-resolution assertions — that the base is the
   `chore(release): vX.Y.Z` commit and not tag reachability — are unchanged and
   remain the guard against a draft that would include already-shipped commits.
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
6. A `macOS ad-hoc signed` job builds the macOS installers and updater
   archives — one architecture per leg, ad-hoc signed, no Apple account
   involved. It runs after the Windows job because that job is what creates the
   draft. See [macOS release packaging](#macos-release-packaging).
7. In parallel, an `Android signed APK` job builds `assembleRelease` and uploads
   `Buildmesh_<version>_android.apk` plus a `.sha256` sidecar to that draft.
   It runs *after* the Windows job, for the same reason. When
   `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEY_ALIAS` and
   `ANDROID_KEY_PASSWORD` are absent it warns and exits 0 instead of failing,
   because the Windows release is already published by that point and failing
   would strand it as a draft. The APK is **not** added to `latest.json`; that
   feed is the Tauri updater and covers Windows and macOS only. See
   [Android release signing](android.md#release-signing-and-distribution).
8. A `Verify updater feed` job, which waits for the macOS audit, reads the draft's real `latest.json` back off the
   release and fails unless every packaged platform is present, signed, and
   pointing at an artifact the release actually carries. This is the last gate
   before a human publishes, and it is the one that catches a macOS archive that
   was uploaded but never made it into the feed.
9. Review the draft release on GitHub and **publish** it. Once published,
   `…/releases/latest/download/latest.json` serves the feed, and running installs
   will show the "Update available" prompt on next launch.
10. Point the [release-notes guide](../releases/README.md) at the next draft
   when one is ready. Do not rewrite the published note to describe later work.
11. **Immediately bump back to the next `-0` version**:
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

That gate is a CI step in `Rust build (compile)`, immediately after
`cargo test --locked --no-run`, and the placement is load-bearing: listing the
tests needs the compiled binary, so it must not be the step that pays to build
it. It used to sit in the non-shard pass ahead of the fuzz smoke, where
`cargo test --lib -- --list` compiled the crate (56.6s on run 38000250072) and
the fuzz smoke then compiled the same target again (48.8s) — 105s of a 185s job
to reach a test run of 0.3s. Running it where the binary already exists also
fails fast: a matrix that does not cover the suite now fails before the seven
shards are dispatched. The gate is still on the merge path, because
`Rust tests + TS bindings` already requires `Rust build (compile)`.

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

`shared-key` alone does not make one entry. rust-cache also folds a hash of
every environment variable named `CARGO*`, `CC*`, `CFLAGS*`, `CXX*`, `CMAKE*`, or
`RUST*` into its key, so two jobs share an entry only when `shared-key` *and*
those variables match. `rust-build` set `CARGO_BUILD_JOBS: "4"` and the shards
did not, and because that variable is invisible to cargo's fingerprint nothing
failed and every job still reported a cache hit: `rust-build` saved
`…-65c564bd-47ff76ef` while all eight downstream jobs read `…-ecb08f1d-47ff76ef`
(run 38000250072). The published compile was therefore read by nobody, and each
job recompiled the crate anyway — ~39s in `rust-build` and ~59s per shard.

The runner has four cores and cargo already parallelises across them, so the
variable was removed from all three jobs rather than added to the other eight.
`tests/agent-infra/rust-cache-key.test.mjs` (`npm run test:agent`) now fails the
build if the Linux Rust jobs' cache-keyed variables drift apart, if their
`RUSTFLAGS`/profile-debug fingerprint inputs differ, or if `CARGO_BUILD_JOBS`
reappears. The failure is otherwise silent: no annotation, no red job, just
minutes per pull request.

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

## macOS release packaging

Issue #2483. A tagged release publishes macOS installers and updater archives
for Apple Silicon and Intel, **without an Apple Developer Program membership,
a Developer ID certificate, an Apple ID, a team, an API key, or notarization**,
and without buying any of them. Linux release artifacts remain the later slice
of issue #835.

### Ad-hoc signing, and what it does not buy

`src-tauri/tauri.macos-release.conf.json` (Apple Silicon) and
`src-tauri/tauri.macos-release-intel.conf.json` (Intel) set
`bundle.macOS.signingIdentity` to `-`, which is Tauri/Tauri Action's ad-hoc
identity: the bundle carries a real, locally-computed code signature with no
certificate behind it. Consequences, stated plainly because they are easy to
misreport:

- The app bundle **is** signed, and `codesign --verify --deep --strict`
  succeeds. The macOS job asserts `Signature=adhoc` rather than accepting any
  successful signature, so a runner that happened to hold a Developer ID
  identity cannot quietly produce a differently-signed release.
- There is **no** Developer ID trust and **no** notarization. Gatekeeper still
  blocks the first launch, exactly as for an unsigned Windows installer under
  SmartScreen. Users right-click the app → **Open** → **Open**, or use System
  Settings → Privacy & Security → **Open Anyway**.
- This is *not* a Gatekeeper bypass. Ad-hoc signing does not remove the
  restriction; it makes the bundle self-consistent and archivable. Any
  documentation claiming the macOS release is "signed" without this caveat is
  wrong.
- The minisign `.sig` beside each archive is the app's own update-integrity
  signature and is unrelated to all of the above, on macOS as on Windows.

The overlay file names are deliberate. Tauri Action auto-merges
`tauri.<platform>.conf.json` for **every** build on that platform, so naming
these `tauri.macos.conf.json` would silently ad-hoc-sign *and* turn on updater
artifacts for the weekly package smoke, which has no signing secret. The
release workflow passes them explicitly with `--config`.

### Architectures and the minimum macOS version

One native build per architecture, because that is what GitHub's runner images
provide:

| Leg | Runner | Rust target | `minimumSystemVersion` |
|---|---|---|---|
| Apple Silicon | `macos-15` (arm64) | `aarch64-apple-darwin` | `11.0` |
| Intel | `macos-15-intel` (x64) | `x86_64-apple-darwin` | `10.13` |

Apple Silicon hardware starts at macOS 11, so Tauri's 10.13 default floor is
not a linkable deployment target for that architecture and the overlay asks for
`11.0`. Intel keeps Tauri's documented minimum.

The Intel release leg currently uses GitHub's `macos-15-intel` runner label.
Check current GitHub runner availability before planning future Intel release
builds.

Those two numbers are what the bundles *declare*: the macOS job reads
`LSMinimumSystemVersion` out of each built `Info.plist` and prints it into the
run log, so the recorded value is evidence rather than an assumption. Whether
an Intel Mac actually running macOS 10.13 launches the app is a question for
real hardware, tracked in issue #2484 — do not claim a minimum macOS version
that no Mac has been run on.

### The updater feed has three writers

`latest.json` is one release asset written by three jobs. `tauri-action`
**merges** it — it re-reads the published asset and keeps its `platforms`
object before adding its own `{os}-{arch}` keys — but it merges by
delete-then-upload, which two concurrent writers would interleave into a lost
update. The workflow therefore serialises every writer:

- `macos-release` has `needs: release`, ordering the Windows writer first and
  putting the macOS legs after the draft exists.
- Its matrix sets `max-parallel: 1`, ordering the two macOS legs against each
  other. This is a correctness constraint, not a speed knob.
- `updater-feed` then downloads the assembled feed and runs
  `scripts/ci/verify-updater-feed.mjs`, which fails unless
  `windows-x86_64`, `darwin-aarch64` and `darwin-x86_64` are all present, signed
  with a well-formed minisign signature, versioned to the tag, and pointing at
  assets the release really carries.

The job that reads a draft needs `contents: write` even though it only reads:
GitHub returns a draft release only to a token with push access, so a read-only
token gets a 404 on the very draft it has to inspect.

`tests/agent-infra/release-macos.test.mjs` pins this shape (one leg per
architecture, no retired image label, `max-parallel: 1`, both feeds required) and
`tests/agent-infra/verify-updater-feed.test.mjs` covers the feed checker itself.

### Checksums

Per-platform manifests, so no job can overwrite another's:

| Manifest | Written by |
|---|---|
| `SHA256SUMS.txt` | the Windows job (EXE/MSI, unchanged) |
| `SHA256SUMS-macos-aarch64.txt` | the Apple Silicon leg |
| `SHA256SUMS-macos-x86_64.txt` | the Intel leg |

The macOS manifests cover both the `.dmg` a person installs and the versioned
`.app.tar.gz` release asset the updater fetches. Intel release assets use the
Tauri `x64` filename suffix while the updater platform key and receipt continue
to identify that CPU as `x86_64`. After Tauri uploads each architecture's
assets, the release job reads `gh release view "$TAG" --json assets`, requires
the expected DMG and updater archive to appear in that listing, downloads those
published assets, and hashes the downloaded bytes into the manifest. If either
expected asset is absent, the packaging job fails before publishing its
manifest. The Android job keeps its own `.sha256` sidecar.

### Before publishing

Packaging evidence is not installed-app runtime evidence. Before publishing a
draft that carries Mac assets, complete and attach a receipt for that exact tag.
The `macos-candidate-audit` job downloads each uploaded DMG back from the draft
on native Apple Silicon and Intel runners. It compares the file with its
architecture's published checksum manifest, mounts it read-only, verifies the
ad-hoc bundle signature and records the app version, executable architecture,
bundle identifier and declared minimum macOS version. It uploads
`MACOS-CANDIDATE-AUDIT-aarch64.json` and
`MACOS-CANDIDATE-AUDIT-x86_64.json` to the draft. This uses `gh` to download the
asset; it does not model a browser's quarantine flag or prove that a person
approved and opened the app from Finder. Those checks stay explicit in the
human receipt.

#### Prepare and validate the receipt

Download the per-architecture audit JSON and checksum manifests from the draft
release into a scratch directory, then copy the
[receipt template](../release-evidence/macos-installed-candidate.template.json)
to an ignored `.tmp/` path:

```sh
TAG=vX.Y.Z
mkdir -p .tmp/macos-release-evidence
gh release download "$TAG" --repo alondero/buildmesh --pattern 'SHA256SUMS-macos-aarch64.txt' --pattern 'SHA256SUMS-macos-x86_64.txt' --pattern 'MACOS-CANDIDATE-AUDIT-aarch64.json' --pattern 'MACOS-CANDIDATE-AUDIT-x86_64.json' --dir .tmp/macos-release-evidence
cp docs/release-evidence/macos-installed-candidate.template.json .tmp/macos-installed-candidate.json
COMMIT="$(git rev-parse "${TAG}^{commit}")"
```

Open `.tmp/macos-installed-candidate.json` and fill it from the browser,
Finder/Gatekeeper, CLI/session, and updater checks below. Replace each
`UNVERIFIED` value only when you have the matching observation and evidence.
After editing the receipt, run the validator:

```sh
node scripts/ci/verify-macos-installed-receipt.mjs .tmp/macos-installed-candidate.json --tag "$TAG" --commit "$COMMIT" --release-dir .tmp/macos-release-evidence
```

Use a clean macOS user account or a test Mac for each native architecture. From
a browser, download the matching DMG and record its SHA-256 alongside the exact
manifest row. Do not remove the download quarantine attribute. Mount the image,
copy Buildmesh to Applications, eject it, then launch it from Finder or Dock.
Record the Gatekeeper prompt and the per-app approval path used; Apple maintains
the [instructions for opening an app from an unidentified developer](https://support.apple.com/guide/mac-help/open-a-mac-app-from-an-unknown-developer-mh40616/mac).
Do not disable Gatekeeper globally. The DMG checksum and ad-hoc signature
establish file consistency, not Apple publisher trust; the minisign signature
belongs to updater archives, not the DMG or Gatekeeper.

Use a harmless test repository and one installed, authenticated supported CLI.
Record its name/version and runtime, that Finder-launched Buildmesh opened the
repository and detected and started the CLI. Confirm a PTY task produced a real
changed-file diff, then close and reopen Buildmesh and verify the project/task
identity remains and the saved conversation resumes. Also confirm a plain
Terminal node is not represented as resumable. App restart can relaunch a
saved harness conversation; it does not
keep the original CLI process alive through app close or reboot. Redact
repository names, credentials, prompts, and private paths from the receipt.

#### Test the updater without the public latest feed

The public Buildmesh bundle identifier and updater key come from the base Tauri
config. For the runtime test, make three temporary copies of the
[updater smoke config template](../release-evidence/tauri-macos-updater-smoke.conf.template.json):
use distinct versions such as `X.Y.Z-smoke.1`, `X.Y.Z-smoke.2`, and
`X.Y.Z-smoke.3`, and point each at one private HTTPS test-feed endpoint that is
not under `releases/latest`. Keep the checked-in bundle identifier and updater
public key unchanged. Do not enable insecure HTTP. Load the existing updater
signing secret through a local secret manager and inject
`TAURI_SIGNING_PRIVATE_KEY` plus `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` into the
build process. The commands below inherit those variables. Never put the key or
password in a config template, command text, shell history, receipt or logs.

Each architecture-specific feed response uses the static Tauri manifest shape;
the `signature` value is the exact text of that updater archive's `.sig` file:

```json
{
  "version": "1.5.1-smoke.2",
  "notes": "Isolated updater smoke",
  "platforms": {
    "darwin-aarch64": {
      "signature": "<contents of Buildmesh.app.tar.gz.sig>",
      "url": "https://<isolated-test-feed>/assets/Buildmesh.app.tar.gz"
    }
  }
}
```

Use `darwin-x86_64` with `Buildmesh.app.tar.gz` and its `.sig` in the Intel response. For the
rejection check, announce smoke version 3 and point to its archive, but replace
the manifest's `signature` value with deliberately invalid text. First serve
the valid version 2 manifest to the installed version 1 app. After it updates,
serve the invalid version 3 manifest to the installed version 2 app.

Build each version from the exact tagged commit, using its matching native
release overlay and temporary updater config. For Apple Silicon, for example:

```sh
npm run tauri -- build --target aarch64-apple-darwin --config src-tauri/tauri.macos-release.conf.json --config .tmp/tauri-macos-updater-smoke-1.json
npm run tauri -- build --target aarch64-apple-darwin --config src-tauri/tauri.macos-release.conf.json --config .tmp/tauri-macos-updater-smoke-2.json
npm run tauri -- build --target aarch64-apple-darwin --config src-tauri/tauri.macos-release.conf.json --config .tmp/tauri-macos-updater-smoke-3.json
```

Use `tauri.macos-release-intel.conf.json` and `x86_64-apple-darwin` on a native
Intel Mac. Each local build writes `Buildmesh.app.tar.gz` and its adjacent
`.sig` under `src-tauri/target/<target>/release/bundle/macos/`. Apple Silicon
uses the same local filename. Copy both files to the isolated HTTPS origin's
`/assets/` path, preserving that local build name in the manifest URLs above.
The release action adds the version and architecture suffix to the published
archive name. Configure the endpoint route from the template
(`buildmesh/<target>/<arch>/<current_version>`) to return the version 2
manifest for installed version 1, then the version 3 manifest for installed
version 2. The response must use `darwin-aarch64` or `darwin-x86_64` as its
platform key and include the exact `.sig` contents for version 2.

Install the first smoke build in a disposable user account and confirm the
in-app update prompt installs and relaunches version 2 with the same workspace
and saved task. Then copy the version 3 archive and signature to `/assets/`,
change the route response to announce version 3 and its archive URL, and
replace the response's signature value with deliberately invalid text.
Confirm the update is rejected and the installed app remains on version 2 with
its workspace intact. These test builds use the tagged source, production
bundle identity and the same updater signing key, while their endpoint and
versions are temporary. They must never publish to or write
`releases/latest/latest.json`.

Fill every required outcome in the receipt as `PASS`, `FAIL` or `UNVERIFIED`,
with a short evidence note. `FAIL` blocks publication. `UNVERIFIED` is allowed
only with its reason and keeps that architecture/workflow out of tested-support
claims. Set `supportClaims.testedArchitectures` and
`supportClaims.testedHarnesses` only for evidence that passed in the receipt;
one working CLI is not evidence for every harness. If native Intel or Apple
Silicon hardware is unavailable, say so in the corresponding fields and keep
the public macOS status at early access without claiming that architecture's
installed workflow or minimum OS has been verified.

Run the receipt validator again after editing, upload the completed file to the
draft as `MACOS-INSTALLED-RECEIPT.json`, and leave the draft unpublished until
the check succeeds and the claims match the outcomes:

```sh
node scripts/ci/verify-macos-installed-receipt.mjs .tmp/macos-installed-candidate.json --tag "$TAG" --commit "$COMMIT" --release-dir .tmp/macos-release-evidence
cp .tmp/macos-installed-candidate.json .tmp/MACOS-INSTALLED-RECEIPT.json
gh release upload "$TAG" .tmp/MACOS-INSTALLED-RECEIPT.json --repo alondero/buildmesh --clobber
```

The validator checks both checksum manifests and both automated audit assets,
rejects any failed scenario or mismatched tag/commit/digest, and rejects support
claims that exceed passing native evidence. Explicitly unverified coverage can
still be published as early access if the maintainer chooses; do not describe it
as tested. The updater feed is not flipped by this job: `latest.json` only
becomes `releases/latest` when the draft is published.

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
4. A complete updater feed — after every packaging job, `Verify updater feed`
   fails the run unless `latest.json` carries a signed, resolvable entry for
   each packaged platform. A failed packaging job already fails the workflow;
   this is what catches the case where every job was green and the merged feed
   is still missing an architecture.

Verifying on the tag rather than trusting the earlier push-to-main run is
deliberate: it is the tag's own SHA that becomes the shipped artifact. The
tag-specific steps (pinned-Codex contract smoke, WSL profile contract) are
`workflow_dispatch`-only and do not run for a release.

Packaging the installer and launching the installed application is a separate
gate: issue #1522 for Windows, issue #2484 for macOS. Until those land, a
release proves the tree is sound and the bundle is produced, not that a clean
install boots.

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

## SmartScreen / Gatekeeper warnings (not Developer ID-signed)

Buildmesh installers are **not** OS-code-signed with a trusted publisher
identity, so first launch shows:

- **Windows** — SmartScreen: *"Windows protected your PC … unknown publisher."*
  Click **More info → Run anyway**.
- **macOS** — Gatekeeper: *"cannot be opened because the developer cannot be
  verified."* Right-click the app → **Open**, then **Open** again; or
  System Settings → Privacy & Security → **Open Anyway**.

This is expected for an internal tool. The macOS bundle *is* ad-hoc signed —
see [macOS release packaging](#macos-release-packaging) — but ad-hoc signing
carries no certificate and confers no Gatekeeper trust, so the prompt is
identical to the unsigned case. Paying for Authenticode / Apple notarization
would remove these warnings; it is deferred (tracked in issue #834) and is
deliberately not required to publish a release. The auto-updater is unaffected:
it verifies updates with its own minisign signature regardless.

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

On a Mac, the same check runs against the updater archive rather than the DMG —
the `.app.tar.gz` is the file the updater actually downloads. The release
manifest uses the versioned asset name for this archive:

```bash
publicKey='RWTd0JrWj/hUt82KDADoxTpTfqs8p6/ay6iht36EPXl2feP892wH1aBG'
minisign -Vm Buildmesh_1.5.0_aarch64.app.tar.gz \
  -x Buildmesh_1.5.0_aarch64.app.tar.gz.sig \
  -P "$publicKey"
shasum -a 256 -c SHA256SUMS-macos-aarch64.txt
codesign --verify --deep --strict /Volumes/Buildmesh/Buildmesh.app   # after install
```

The command must report a valid signature before installation. Compare the
SHA-256 output with the matching filename in the published manifest. Never
replace the public key with a key copied from an untrusted release comment. The
checksum file is a second download-integrity check; it does not replace the
cryptographic signature.
