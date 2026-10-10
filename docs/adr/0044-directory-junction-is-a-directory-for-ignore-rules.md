# 44. A Windows directory junction is a directory when ignore rules are matched

Status: accepted

## Context

Buildmesh asks libgit2 one question in several places: is this worktree dirty?
`git::primitives::is_dirty` feeds the circuit wrap-up gate
(`circuit::verification::observe_wrapup_git_state`), the worktree-close
data-safety gate, mesh health, and the prune badge, and two further
`repo.statuses()` walkers list changed files and their count in `commands::git`.
They read the same libgit2 answer.

libgit2's status walker does not resolve a Windows reparse point when deciding
whether an untracked path is a directory, so a **directory-only** ignore rule
does not match it. `src-tauri/.gitignore` rule 3 is `/target/` — anchored *and*
directory-only — and `CLAUDE.md` points an agent at a `node_modules` junction in
a fresh worktree. Junctioning `src-tauri/target` at the main checkout's cargo
cache is the obvious extension, and saves a ~5 minute rebuild. Doing it made the
wrap-up gate report a pristine worktree as dirty on each of run-414's two
retries, so the agent was handed a wrap-up correction it could not possibly
satisfy (issue #2193).

Measured against the live tree rather than inferred: `git status --porcelain` is
empty while libgit2 reports `WT_NEW src-tauri/target`. `node_modules` and `dist`
were junctions too, but their rules carry no trailing slash, so they matched
regardless of entry type and were correctly ignored. Only `/target/` failed.

## Decision

`is_ignored_entry` in `git::primitives` is the one ignore verdict for the
`repo.statuses()` walkers listed above, plus the
`env::test_helpers::repo_is_dirty` fixture assertion. It keeps libgit2's own
`Status::is_ignored()` and adds exactly one case: an entry that is untracked
(`WT_NEW`), sits at a Windows reparse point carrying
`FILE_ATTRIBUTE_DIRECTORY`, and that `Repository::is_path_ignored` reports as
ignored. `is_path_ignored` resolves the reparse point and matches the rule as a
directory, so one extra query per junction restores parity with the CLI.

Invariants:

1. **Re-check, never override.** The extra query can only *add* an ignore
   verdict. A path libgit2 reported non-ignored is still dirty whenever the
   ignore stack does not match, so no real work is hidden.
2. **Untracked entries only.** A modified tracked path is dirt whatever the
   ignore rules say, and re-checking it would only spend a query.
3. **Directories only.** A `dir/` rule is about a directory, and a symlink to a
   *file* carries `REPARSE_POINT` without `DIRECTORY`. Requiring both
   attributes keeps the guard on the rule it was written for.
4. **Windows only.** On Unix there is no reparse concept, so the guard leaves
   libgit2's verdict exactly as it found it rather than second-guessing a
   platform it cannot measure from this codebase.

The gates keep failing closed: an unreadable repo or ignore stack still means
dirty, and `repo_error` still short-circuits the invented reasons.

## Alternatives considered

- **Give the wrap-up gate `git status` CLI parity** — reuse
  `dirty_paths_mirrors_git_status`, or shell out to `git status`. Rejected:
  that helper reads the same libgit2 status, so it shares the defect; and a
  subprocess per verification buys nothing over one `is_path_ignored` query.
- **Keep the workaround and document it** (do not junction `target/`).
  Rejected: it leaves the gate calling a clean tree dirty for everyone who
  already junctions it, which is exactly the failure ADR-0011 exists to
  prevent.
- **Route every entry through `is_path_ignored`.** Rejected: it spends an extra
  ignore query per status entry to re-derive a verdict libgit2 already made,
  and it drags the strict data-safety gate onto the parity path.

## Consequences

A junctioned `target/` no longer makes the wrap-up gate, mesh health, the prune
badge, the worktree-close gate, or the Probe panel's changed-file list report a
clean worktree dirty. The submodule contract is unchanged: `is_dirty` still
ignores `submodule.<path>.ignore` and still reports submodule dirt, because that
is what the data-safety consumers depend on.

Verification evidence: `git::primitives::tests::junction_tests` (an ignored
junction is clean, an *un*ignored junction stays dirty, a modified tracked file
stays dirty), `circuit::verification::tests::observe_wrapup_git_state_clean_when_target_is_a_junction`,
and `commands::git_tests::test_junction_under_directory_only_rule_is_not_reported`
for the Probe panel's two surfaces. All three are Windows-gated — a directory
junction is a Windows construct, so the Linux merge gate cannot exercise them.

## Links

- Issue #2193 (refs #2191, run-414)
- [ADR-0011](0011-autopilot-session-wrapup.md) — wrap-up is prompt-driven, so a
  wrong wrap-up correction is the failure mode this record exists to prevent
- [ADR-0007](0007-extract-git-module.md) — `git/` owns all direct `git2` access
- [ADR-0033](0033-ff-pull-blocked-only-by-overlapping-local-changes.md) — the
  `git status`-parity helper that now shares this ignore verdict
- [circuits](../development/circuits.md) — the wrap-up gate this unblocks