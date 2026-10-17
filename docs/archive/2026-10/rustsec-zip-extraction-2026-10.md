# `zip` extraction advisories and the updater feed (issue #2438), 17 October 2026

Historical investigation, not the current contract. The maintained policy is
[RustSec advisory policy](../../development/supply-chain.md#rustsec-advisory-policy),
and the reusable rule is its
[When no fixed release exists](../../development/supply-chain.md#when-no-fixed-release-exists)
section. This file records the evidence behind one application of that rule, so
the reasoning can be re-derived rather than re-argued. It is a snapshot; every
"as checked" claim below can change without a commit in this repository.

## The finding

`cargo audit` reported three vulnerabilities in `zip@4.6.1`, none of which this
repository's policy allows to be excepted:

- `RUSTSEC-2026-0367` — insecure preservation of SUID/SGID bits during extraction
- `RUSTSEC-2026-0368` — extraction can create symlinks targeting paths outside the destination
- `RUSTSEC-2026-0369` — extraction can apply Unix permissions through symlinks without confining the target

All three list `>=9.0.0` as the solution.

## How `zip` got into the tree

It was never a direct dependency. `cargo tree -i zip` gave the whole path:

```
zip v4.6.1
└── tauri-plugin-updater v2.13.3
    └── buildmesh
    └── via tauri-plugin-updater feature "default" -> feature "zip"
```

The plugin declares it Windows-only, under
`[target.'cfg(target_os = "windows")'.dependencies.zip]`, and its `zip` feature is
part of `default`.

## Why no upgrade was available

As checked on 2026-10-17 against the crates.io API, every published version of
`tauri-plugin-updater` requires `zip ^4` — including `2.14.0` (highest stable) and
`3.0.0-alpha.3` (highest pre-release). `cargo search` does not surface `2.14.0`,
so the version list has to be read from the API rather than the search rank.

On the tree as it stood before the fix, `cargo update -p zip --precise 9.0.4`
(run from `src-tauri/`; from the repository root Cargo fails earlier, on the
missing manifest) produced:

```
error: failed to select a version for the requirement `zip = "^4"`
candidate versions found which didn't match: 9.0.4
location searched: crates.io index
required by package `tauri-plugin-updater v2.13.3`
    ... which satisfies dependency `tauri-plugin-updater = "^2"` (locked to 2.13.3)
```

After the fix there is nothing left to update: `zip` is absent from
`Cargo.lock`, and the same command reports that the package specification matched
nothing.

## Establishing that the feature was unused

This is the part that decides whether the fix is safe, and it was the part an
earlier revision of the pull request got wrong. The published v1.4.1 feed maps
every Windows key to a non-archive artifact:

| Feed key | Release asset | Name |
| --- | --- | --- |
| `windows-x86_64` | 638495693 | `Buildmesh_1.4.1_x64_en-US.msi` |
| `windows-x86_64-msi` | 638495693 | `Buildmesh_1.4.1_x64_en-US.msi` (same asset) |
| `windows-x86_64-nsis` | 638495763 | `Buildmesh_1.4.1_x64-setup.exe` |

With the `zip` feature off, `extract()` (`tauri-plugin-updater` `updater.rs`,
around line 1050) goes straight to `extract_exe()`, which handles `.exe` and
`.msi` natively and never opens an archive. Because no key resolves to a ZIP,
the archive path is unreachable whichever key the updater settles on — the
argument does not depend on the installer-specific key being tried first.

An earlier revision of this work claimed the generic `windows-x86_64` key was a
ZIP with no published asset. That was inferred from the Tauri CLI's convention of
emitting an archive under the generic key and was never checked against the
release API. It was wrong; the key resolves to the MSI. The lesson generalises:
derive the feed mapping from `gh api repos/alondero/buildmesh/releases/tags/v1.4.1`
rather than from the bundler's documented convention.

## Residual scope, and why the feature must not simply be turned back on

The feature also gates Linux AppImage `.tar.gz` extraction (`updater.rs`, around
line 1179). Buildmesh publishes no Linux release or AppImage artifact, so nothing
regresses today. All the plugin's Windows updater types are `#[cfg(windows)]`, so
that block is the feature's only Linux effect, and `std::io::Cursor` stays used
elsewhere in the file, so disabling the feature raises no unused-import warning.

That residual scope is **not** an argument for re-enabling the feature. The two
are one switch: in `tauri-plugin-updater` 2.13.3 it is

```
zip = ["dep:zip", "dep:tar", "dep:flate2"]
```

so turning it on to reach Linux AppImage extraction also puts the pinned
`zip = "4"` back into `Cargo.lock` and into the Windows build, which is exactly
what `RUSTSEC-2026-0367`, `-0368` and `-0369` describe. The Windows extraction
path becomes reachable again even though no Windows feed key needs it today, and
the advisory gate fails. The same applies to serving a real `.zip` update
artifact.

Adding Linux support, or a `.zip` artifact, therefore needs a safe extraction
path or a fixed dependency first — a plugin release that splits the feature or
raises the `zip` floor to 9.x, or extracting the AppImage outside the plugin.
Only then is the feature worth reconsidering.

## What to re-verify before repeating any of this

- the crates.io version list for `tauri-plugin-updater`, and each version's `zip` requirement
- whether the feature still gates both the Linux and the Windows extraction paths
- the `solution` field on each `RUSTSEC-2026-0367/0368/0369` advisory
- the published `latest.json` platform-to-asset mapping for the current release