# Release notes

Status: current

This directory contains the user-visible release notes for Buildmesh. Each
release has one file named `vX.Y.Z.md`; the matching Git tag and file are the
release's versioned source of truth.

The next release is [v1.4.0](v1.4.0.md); later notes are drafted at release time
(see [Drafting a release](#drafting-a-release)).

## Drafting a release

Release notes are drafted at release time, not per pull request. A single file
that every PR appended to conflicted on nearly every parallel change, so
contributors no longer edit it:

1. Write a Conventional Commit subject and body that a reader could turn into a
   release-note line.
2. At release time the maintainer runs `npm run release:notes`, which drafts
   `vX.Y.Z.md` from the Conventional Commits merged since the previous release,
   then curates it. The version defaults to the manifest version without its `-0`
   suffix.

The base is the `chore(release): vX.Y.Z` commit for the highest already-released
version below the one being prepared. Release tags point at the pre-merge commit
on the release branch and so are not ancestors of `main`, which is why plain tag
reachability is not used. Check the `Generated from … commits since …` header
before curating: it names the resolved base and the size of the range, so a wrong
range is visible immediately.

Preview on stdout with `npm run release:notes`; add `--write` to create the file
(`--write` refuses to overwrite an existing draft — pass `--force` to regenerate
over one, or curate it in place).

Keep only changes users need to know about:

- features and meaningful workflow changes;
- fixes that change user-visible behavior;
- security, privacy, migration, or breaking changes; and
- known limitations that affect the release.

Drop documentation-only corrections, refactors, tests, CI changes, and agent
infrastructure unless they materially change a user's experience. Those changes
belong in the relevant documentation, PR evidence, or commit history.

The draft is reviewed in the release pull request, then passed to the GitHub
Release created by the tag-triggered release workflow. After publication, the
versioned file is historical release documentation and should not be rewritten
to describe later work.

See the [release procedure](../development/releasing.md) for versioning,
tagging, signing, and publication steps.
