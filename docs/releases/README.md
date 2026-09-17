# Release notes

Status: current

This directory contains the user-visible release notes for Buildmesh. Each
release has one file named `vX.Y.Z.md`; the matching Git tag and file are the
release's versioned source of truth.

The current planned release is [v1.3.0](v1.3.0.md). Update this link when a
different release becomes the active draft.

## Drafting a release

Maintainers create the file when the next release version is known. Contributors
add only changes that users need to know about:

- features and meaningful workflow changes;
- fixes that change user-visible behavior;
- security, privacy, migration, or breaking changes; and
- known limitations that affect the release.

Do not add documentation-only corrections, refactors, tests, CI changes, or
agent infrastructure unless they materially change a user's experience. Those
changes belong in the relevant documentation, PR evidence, or commit history.

Release notes are reviewed in the normal pull request, then passed to the
GitHub Release created by the tag-triggered release workflow. After publication,
the versioned file is historical release documentation and should not be
rewritten to describe later work.

See the [release procedure](../development/releasing.md) for versioning,
tagging, signing, and publication steps.
