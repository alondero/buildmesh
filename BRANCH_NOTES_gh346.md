# Branch: gh346-pathinvalidatedcache-replace-has-null-symbol

Session-only artifact. Not part of the shipped product.

## Outcome

Issue [#346](https://github.com/alondero/buildmesh/issues/346) ("pathInvalidatedCache:
replace `HAS_NULL` Symbol sentinel with a two-Map pattern") was **already
resolved** when this branch was created:

- The fix landed in PR #906 (`eb5e305 refactor(pathInvalidatedCache): replace
  HAS_NULL Symbol sentinel with two-Map presence pattern`), merged
  2026-07-18T20:30:06Z on the same day the branch was cut.
- `git diff main...HEAD --stat` is empty — the branch is byte-identical to
  main.
- `grep HAS_NULL` across the repo returns zero matches.

## What this branch did

1. Verified the refactor is in place (`known: Map<K, true>` + `values: Map<K, V>`,
   `read` = `known.has(key) ? (values.get(key) ?? null) : undefined`).
2. Ran `scripts\check.ps1 unit` → **1661 / 1661 unit tests passed** (GREEN).
3. Ran `scripts\check.ps1 all` → unit + rust + build all GREEN.
4. Closed the orphaned issue (#346 was still `state: OPEN` with
   `closedByPullRequestsReferences: []` despite the merged fix). Now
   `state: CLOSED, stateReason: COMPLETED`.

## Why this file exists

This branch has no diff vs main, but the wrap-up workflow expects a PR to
exist. This file is the only diff and gives the PR a target commit. Delete
the branch (and this file) on cleanup.
