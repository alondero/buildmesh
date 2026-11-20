# ADR 0062: Resolve Probe blockers from the open issue feed

Status: accepted

## Context

The Issues Probe shows which work is blocked as users scan the list. GitHub
issue bodies can name blockers, and the Probe already fetches the current
repository's open issues, including pagination and completeness metadata.

## Decision

Resolve blocker references against the open issues already loaded for the
repository. The warning chip identifies the first matching blocker, with a
count for additional matches when needed. The warning stripe supports
scanning while the chip offers the link. The next feed refresh reflects
blockers that have closed.

## Alternatives and consequences

Fetching each referenced issue would allow more complete resolution but
would add requests and could delay the list or hit GitHub's limits. The
local lookup reuses the same snapshot and makes its scope predictable.
The feed completeness note reports limits on this snapshot. External
references need to retain repository identity so they cannot accidentally
match a local issue with the same number.

## Links

- [GitHub list completeness](../development/probe-ui.md#github-list-completeness)
