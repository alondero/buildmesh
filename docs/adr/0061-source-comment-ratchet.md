# ADR 0061: Ratchet source comments against the Git comparison base

Status: accepted

## Context

Buildmesh has source comments across application code, tests and build
configuration. A blanket failure on all existing comments would block
unrelated work during the cleanup.

## Decision

The comment lint compares the text and occurrence count of comments in each
changed source file against the same path in the Git comparison revision.
Local lint checks both the worktree and staged content and defaults to `HEAD`.
The development harness and pull request CI pass their comparison revisions.
A new file starts with no comment allowance.
Moving a comment to another file, editing it or introducing a duplicate fails
the lint. Once a removal reaches the base, reintroducing that comment fails.

Committed generated bindings, Gradle's distributed wrapper scripts and
documentation are outside the source lint. Existing tool directives can be
removed along with the settings or code that depend on them. Shell shebangs
retain their interpreter-selection role.

## Alternatives and consequences

A repository-wide count would let new comments replace deleted ones. A checked
in list of every existing comment would give a per-file baseline, with a large
list to store and review and entries to update on each removal. Using Git
preserves the per-file limit and makes the comparison revision part of
verification. Renaming a file with comments requires removing or relocating
the comments as part of the rename.

## Links

- [Contributor verification](../development/README.md#source-comment-lint)
