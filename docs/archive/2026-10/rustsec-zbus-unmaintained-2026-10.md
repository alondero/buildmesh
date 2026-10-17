# `zvariant` and `zbus_names` unmaintained advisories (issue #2444), 17 October 2026

Historical investigation, not the current contract. The maintained policy is
[RustSec advisory policy](../../development/supply-chain.md#rustsec-advisory-policy),
and the reusable rule is its
[When no fixed release exists](../../development/supply-chain.md#when-no-fixed-release-exists)
section. This file records the evidence behind one application of that rule, so
the reasoning can be re-derived rather than re-argued. It is a snapshot; every
"as checked" claim below can change without a commit in this repository.

Unlike the `zip` case ([issue #2438](rustsec-zip-extraction-2026-10.md)), this
could **not** be fixed by removing the crate: the dependency that carries it has
no feature switch and no upstream release has moved past it. The three advisories
are therefore recorded as reviewed exceptions, each with an owner, a rationale and
a `reviewBy` date in `.github/rustsec-exceptions.json`.

## The finding

Three `unmaintained` warnings, all published on 2026-10-16 against the same
upstream issue (`z-galaxy/zbus#2043`):

| Advisory | Crate | Version | Upstream disposition |
| --- | --- | --- | --- |
| `RUSTSEC-2026-0370` | `zvariant` | 5.15.0 | merged into `zbus` 6.0.0 as its `wire` module |
| `RUSTSEC-2026-0371` | `zbus_names` | 4.3.4 | merged into `zbus` 6.0.0 as its `names` module |
| `RUSTSEC-2026-0372` | `zvariant_derive` | 5.15.0 | derive macros provided by `zbus` 6.0.0 |

All three carry `patched = []`. These are not bug reports with a fixed release
behind them: the crates were folded into `zbus` 6.0.0 and will not ship another
version at all, so **no version bump to any of the three clears the advisory**.
The `cargo audit` exit code is non-zero on warnings, and this repository's policy
fails an unreviewed warning, so the job went red rather than silently tolerating
them.

## How they got into the tree

None is a direct dependency — Buildmesh never names `zbus`, `zvariant`,
`zbus_names` or `zvariant_derive` in its own code. `cargo tree -i zvariant
--target all` gives the whole path:

```
zvariant v5.15.0
└── zbus v5.19.0
    └── tauri-plugin-opener v2.7.1
        └── buildmesh
        └── via tauri-plugin-opener feature "default"
```

Note the `--target all`. A plain `cargo tree -i zvariant` reports "nothing to
print", because on a Windows host the chain does not exist for the current
target. The dependency is declared target-conditionally in the plugin's
`Cargo.toml`:

```toml
[target.'cfg(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))'.dependencies.zbus]
version = "5.9"
```

## Why no upgrade was available

Two separate reasons, and both had to be checked — either alone would have been a
misleading reason to leave an advisory unaddressed.

**There is no feature switch.** The declaration above is a non-optional
`[target.'cfg(...).dependencies]` entry, not an optional dependency behind a
feature. `default-features = false` on `tauri-plugin-opener` therefore cannot
remove it, which is exactly the lever that solved the `zip` case. Unlike `zip`,
`zbus` cannot be dropped by turning a feature off.

**No published plugin release has moved to zbus 6.** As checked on 2026-10-17
against the crates.io dependencies API:

| `tauri-plugin-opener` | `zbus` requirement |
| --- | --- |
| 2.7.1 (in `Cargo.lock`) | `^5.9`, non-optional, Linux/BSD |
| 2.7.2 (highest stable) | `^5.9`, non-optional, Linux/BSD |
| 3.0.0-alpha.3 (highest pre-release) | `^5.9`, non-optional, Linux/BSD |

So bumping to the newest stable `2.7.2` — which a dependency-update habit would
reach for first — resolves nothing and only churns the lockfile. The Tauri 3 alpha
would drag in `tauri 3.0.0-alpha.4` and still not clear the advisory, which puts
it far outside what a maintenance review should be doing. Read the dependencies
API rather than `cargo search`: the search rank does not reliably surface the
newest patch release.

`[patch]` was rejected rather than tried. Forcing `zbus` 6 in place of the 5.19.0
the plugin resolves against would be a build break, not a fix — zbus 6 is the
release that absorbed these crates, so it is a different API surface — and the
failure would surface only on Linux, the one platform no pull-request check
compiles.

## Why the path is not in a released artifact

The advisories are maintenance warnings rather than vulnerabilities, so what
matters is whether the unmaintained code does anything this repository ships. It
is reached through `tauri-plugin-opener`, which Buildmesh uses for real,
user-visible behaviour: `open_path` for "open in editor"
(`src-tauri/src/commands/file_tree.rs`) and `openUrl` across the frontend
(`SafeLink`, `MeshItem`, `PrPill`, `TerminalLinkRouter` and others). Removing the
plugin would mean reimplementing external-URL and file launching, which is a
product change rather than a dependency review.

What Buildmesh actually distributes is Windows installers (`.exe`, `.msi`) and a
signed Android APK; `release.yml` builds the Windows bundle on `windows-latest`
and asserts on those two extensions. So the plugin's Linux code path, and with it
this whole subtree, is not linked into any released artifact.

Stated precisely, because the broader claim would be false: the code **is**
compiled on Linux, and not only occasionally. `verify.yml` runs `Rust build
(compile)`, the `Rust tests` shards, `Rust export, doc, and integration tests` and
`Rust tests + TS bindings` on `ubuntu-latest`, so whenever those jobs run they
compile this subtree and would catch it breaking. They are change-gated
(`needs: changes`) rather than unconditional, so "compiled on every pull request"
would overstate it; the honest claim is that a Linux compile is part of normal CI
rather than a rarity. The weekly `package-smoke` job additionally runs a full
`npm run tauri build` on `ubuntu-latest` to check that every configured native
bundle still builds, and deliberately does not upload the artifacts.

The claim that matters is therefore about what **ships**, not about what is ever
built. An earlier draft of this file said the subtree was "only compiled by the
weekly package-smoke job", which was wrong in the direction that flatters the
decision: the more accurate and more awkward fact is that Linux is continuously
compiled and tested while shipping nothing.

That asymmetry is the actual justification for carrying reviewed exceptions rather
than fixing the crate. An upstream that has stopped releasing cannot be left
behind by a release, and no artifact Buildmesh publishes contains this code, so
the cost of an unmaintained subtree is currently zero. It is equally the condition
that should prompt somebody to revisit these exceptions: a future Linux release
would put this subtree in users' hands.

`zvariant_derive` is additionally a proc-macro, so it is build-time only and
cannot affect run-time behaviour even there.

## What to re-verify before repeating any of this

- the crates.io version list for `tauri-plugin-opener`, and each version's `zbus`
  requirement (the trigger to drop these exceptions is a `^6` or later)
- that `zbus` is still a non-optional target dependency rather than an optional
  one, which would restore the feature-switch route
- the `patched` field on `RUSTSEC-2026-0370`, `-0371` and `-0372`
- the platforms `release.yml` publishes, if a Linux release is ever added