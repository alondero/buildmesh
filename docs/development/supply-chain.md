# Dependency and workflow supply-chain controls

Status: current

This is the contract for the third-party code this repository executes: the
npm and Cargo dependency trees, and the GitHub Actions every workflow runs. It
covers what is checked, on which cadence, what fails a pull request, and how a
warning is reviewed and retired.

The threat is not hypothetical for this repository specifically. The release
job holds `contents: write` and the updater signing keys
(`TAURI_SIGNING_PRIVATE_KEY`), so a mutable `uses:` tag in a workflow that the
release path reaches is remote code execution with the authority to sign an
update that every installed client will trust.

## What runs, and where

| Control | Local command | CI job |
|---|---|---|
| Action pins (with SHA resolution) | `npm run check:actions` | `Verification / Supply chain` |
| npm advisory policy | `npm run check:audit` | `Verification / Supply chain` |
| RustSec advisory policy | `npm run check:audit:rust` | `Verification / Supply chain` |

The CI job is not one of the three required status checks, so a supply-chain
failure blocks the release (the `release` job `needs: verify`, which runs the
whole `verify.yml` graph) rather than the merge. Promoting it to a required
merge check is a deliberate future step, not an omission — see
[Not yet a required check](#not-yet-a-required-check).

## Repository settings

These are repository settings rather than files, so they do not travel with a
clone and cannot be asserted by a test. They were confirmed enabled while this
policy was written:

- **Dependency graph** — enabled.
- **Vulnerability alerts** — enabled. The endpoint returns "disabled" when off;
  `gh api repos/<owner>/<repo>/vulnerability-alerts` is the check.
- **Dependabot security updates** — enabled. These are separate from the version
  updates in `.github/dependabot.yml` precisely so a security fix does not wait
  for the weekly batch.

Verify with:

```powershell
gh api repos/alondero/buildmesh --jq .security_and_analysis
gh api repos/alondero/buildmesh/vulnerability-alerts   # HTTP 404 when disabled
```

Secret scanning and push protection were already enabled and are unchanged.

## Dependabot

`.github/dependabot.yml` runs npm, Cargo, and GitHub Actions weekly on Monday
morning, each **grouped**. Ungrouped npm on a repository this size produces
dozens of single-bump pull requests a week, and every pull request pays the
frontend quality gate (and, for a Rust or workflow change, the Rust graph), so
the review cost is real. Grouping keeps one reviewable pull request per
ecosystem while the gates keep the safety. Cargo is split by semver level so a
major bump — the kind that changes an API — arrives alone rather than hidden in
a batch.

The Actions group matters most for this policy: Dependabot rewrites a
SHA-pinned `uses:` **and** the `# vX.Y.Z` comment beside it, so pinning does not
mean updates become manual.

## Action pinning

Every third-party `uses:` reference is pinned to a full 40-character commit
SHA with a version comment:

```yaml
- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7
```

`scripts/check-action-pins.mjs` enforces the rule across `.github/workflows/*.yml`
and `.github/actions/*/action.yml`:

- local (`./…`) and `docker://` references are exempt;
- a tag **or a branch** reference fails — a branch is mutable by definition, so
  pinning to one is a pin that can move under a merge;
- a SHA without a version comment fails, because the comment is what makes the
  pin readable and is what Dependabot updates;
- `--verify` (used by `npm run check:actions` and CI) resolves each pinned SHA
  against the GitHub API, so a typo or truncated SHA fails here rather than at
  run time.

To pin a new action, resolve its tag (following `object.url` once for an
annotated tag):

```powershell
gh api repos/<owner>/<repo>/git/ref/tags/<tag>
gh api repos/<owner>/<repo>/git/tags/<annotated-sha>   # only if type == "tag"
```

The gate does **not** require the pin to be the latest tag, or on the default
branch. Both change constantly, and a gate that fails for those reasons trains
people to bypass it. Age and provenance are judged when the pin is written or
bumped.

`dtolnay/rust-toolchain@stable` is the one action pinned to a moving channel
rather than a release tag; it is still pinned to an immutable commit SHA and
labelled `# stable`.

### Exceptions

An action that genuinely cannot be pinned (no releases, for example) goes in
`.github/action-pin-allowlist.txt` as `owner/action@ref`, with a `#` comment
explaining why. Nothing currently does.

## npm advisory policy

`scripts/check-npm-audit.mjs` is two-tiered, and the tier is the point:

| Scope | Fails at | Why |
|---|---|---|
| Production (`--omit=dev`) | `moderate` and above | These packages ship inside the packaged application and run with user data. |
| Development | `high` and above | Dev packages are not in a shipped artifact, but they execute at build time and can be fed attacker-influenced input (install scripts, a parser reading a crafted fixture). |

Dev-only `low` and `moderate` advisories are **reported, not failed**. Failing
them is what pushes people toward `--audit-level=0` or `--force`, which
disables the gate entirely.

### How dev-only status is determined

Not from the report. `npm audit --json` carries **no `dev` flag** on either a
vulnerability or its `via` entries — verified against npm 11, where the keys are
`name`, `severity`, `isDirect`, `via`, `effects`, `range`, `nodes`,
`fixAvailable`. A gate that guessed here would silently apply the stricter
production threshold to every advisory and quietly contradict its own policy.

So npm is asked twice and the difference is the answer: `npm audit --omit=dev`
reports exactly the advisories affecting the production tree, so a package
present only in the unfiltered report is dev-only. One `npm run check:audit`
invocation performs both runs and applies both tiers.

`tests/fixtures/npm-audit-vulnerable.json` is real captured npm output from a
throwaway project with a vulnerable production dependency (`lodash` 4.17.15)
and a vulnerable dev-only one (`minimist` 0.0.8), so the tiering is tested
against the shape npm actually emits.

An advisory report shape the gate does not recognise is a **failure**, not a
clean result: an unreadable audit is not a clean audit.

### Exceptions

There are none. Unlike the RustSec gate below, `check-npm-audit.mjs` has no
allowlist: an npm advisory is fixed with `npm audit fix` or a deliberate
upgrade. This is a real asymmetry, not an oversight, and it is acceptable
because npm advisories in the production tree have a fix path in essentially
every case, whereas the Rust warnings that remain are largely unfixable
transitive crates. If an npm advisory ever needs a time-bound exception, add
the allowlist then — rather than shipping an unused mechanism now.

## RustSec advisory policy

`cargo audit` splits findings into *vulnerabilities* (exploitable) and
*warnings* (unmaintained, unsound, yanked). `scripts/check-rust-advisories.mjs`
treats them differently on purpose:

- **A vulnerability always fails.** There is no exception mechanism. A reviewed
  exception to a known vulnerability is precisely what this gate exists to
  prevent, so an advisory in this class must be fixed by upgrading the crate.
  This is why `RUSTSEC-2026-0285` (rustls) was fixed rather than recorded when
  this policy was written.
- **A warning fails unless reviewed** in `.github/rustsec-exceptions.json` with
  an `owner`, a `rationale`, and a `reviewBy` date. An unlisted warning fails as
  *unreviewed*, which is a different and louder failure than *known and
  accepted*.
- **An expired review fails.** Past `reviewBy`, somebody must re-affirm the
  rationale or fix the crate, so an exception cannot become permanent by
  neglect.
- **An exception for an advisory no longer reported is stale** and fails, so the
  file cannot accumulate rationales describing a dependency tree that no longer
  exists.

A yanked crate has no RustSec advisory document, so those entries are matched by
`crate@version` instead of advisory ID.

### Recording an exception

```json
{
  "id": "RUSTSEC-2025-0141",
  "crate": "bincode",
  "version": "1.3.3",
  "kind": "unmaintained",
  "owner": "maintainers (transitive)",
  "rationale": "Why the advisory does not apply here, and what would change that.",
  "reviewBy": "2027-01-15"
}
```

Write the rationale against the actual dependency tree, not the advisory text.
The useful question is "why is this reachable-but-harmless here?", and the
answer usually names where the crate comes from (`cargo tree -i <crate>`) and
which code path avoids the vulnerable behaviour.

## Workflow permissions

Every workflow declares an explicit top-level `permissions:` block, so no job
inherits a read-write repository default. Job-level permissions *replace* the
top-level block rather than adding to it, which is what keeps `verify` in
`release.yml` from escalating to `contents: write` through the release job
alongside it.

- `verify.yml`, `build.yml`, `ci-retry.yml`, `android.yml`, `release.yml`:
  `contents: read` at the top level.
- `release.yml`'s `release` job alone holds `contents: write`, for creating the
  GitHub Release and uploading artifacts.
- `build.yml`'s `alert-on-failure` holds `issues: write` to open the weekly
  failure-tracking issue.

Secrets stay away from untrusted pull-request code by trigger, not by
convention: `release.yml` runs only on a `v*` tag, so the signing keys are never
in scope for a pull request. There is no `pull_request_target` trigger in this
repository, which is what would otherwise be the way to run fork code with
repository secrets.

## Not yet a required check

`Verification / Supply chain` is not one of the three required status checks in
`docs/development/releasing.md`, so it blocks a **release** (the release job
needs the whole verify graph) but not a **merge**. The reason is cost: the job
clones the RustSec advisory database and runs `npm audit` against the registry,
so it is minutes rather than seconds, and making it required would put it on the
critical path of every pull request including documentation-only ones.

If it is promoted to required, update the `main: verified merges only` ruleset
and the required-checks table in `docs/development/releasing.md` in the same
commit, and give the job a `changes`-based skip for pull requests that touched
neither a lockfile, a manifest, nor a workflow.

## Local use

The three gates need no arguments and no network beyond the audit itself:

```powershell
npm run check:actions      # pins, with SHA resolution (needs gh)
npm run check:audit        # npm, both tiers (runs the audit twice by design)
npm run check:audit:prod   # npm, production dependencies only
npm run check:audit:rust   # RustSec policy (needs cargo-audit)
```

`node --test tests/agent-infra/supply-chain-gates.test.mjs` covers the policy
itself, including deliberately unpinned and deliberately vulnerable fixtures
that must fail — a gate only ever run against a clean tree is a gate nobody has
watched reject anything.