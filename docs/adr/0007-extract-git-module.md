# 7. Extract a `git` Module for Shared git2 Access

Status: accepted

Buildmesh consolidates its direct `git2` usage behind a new top-level `src-tauri/src/git/` module. The deep logic (worktree lifecycle, mesh health, sync, and the low-level git2 primitives they share) moves there; the `#[command]` functions stay thin adapters in `commands/` (so the `lib.rs` handler registration is unchanged); and `env/mod.rs` is left as a focused path/environment-conversion module.

## Context

git2 access grew organically into whichever module first needed it. As of today the same primitives are re-derived in four or more places, and the copies have **already diverged**:

- **"repo has uncommitted changes"** exists in `commands/git.rs` (`repo_is_dirty`), `env/mod.rs` (inline in `fetch_origin`), `services/agent_node.rs` (`repo_has_uncommitted`), and `commands/prune.rs` (`repo_has_uncommitted`). Two of them filter on `!is_ignored()`; the other two add `&& != Status::CURRENT`. The error fallback also diverges: the sync/close gates fail *closed* (`unwrap_or(true)` → "treat as dirty, don't risk work"), while the display paths fail *open* (`unwrap_or(false)` → "treat as clean"). Same question, four implementations, two answers, two safety directions.
- **ahead/behind vs upstream** (`branch_upstream_name` → `find_reference` → `graph_ahead_behind`) is hand-rolled in `get_git_branch_status`, `count_commits_behind`, `compute_unpushed` (all `commands/git.rs`), `count_commits_behind_upstream` (`env/mod.rs`), `head_has_unpushed_or_unmerged_commits` (`services/agent_node.rs`), and per-branch in `collect_prune_info` (`commands/prune.rs`).
- **short SHA** (`short_id` → `from_utf8_lossy`) appears three times in `commands/git.rs` alone; **HEAD branch name** (`symbolic_target` → strip `refs/heads/`) twice; **path normalisation for comparison** twice.

Three of these duplications carry comments that explicitly justify the copy-paste as a way to keep the command modules decoupled (`commands/git.rs:687`, `commands/git.rs:757`, `env/mod.rs:763`). That reasoning is the symptom: the modules are decoupled *from each other* but every one is hard-coupled to `git2` directly, with no shared seam to own the primitive. The divergence above is the cost that was already paid.

Two related smells fall out of the same root cause:

- **Two near-identical sync paths.** `env::fetch_origin` (auto-sync on spawn, typed `FetchOutcome`/`FetchError`) and `commands::git::git_sync` (manual, `GitSyncResult`) both do fetch → count-behind → `pull --ff-only`. `fetch_origin` passes `--no-rebase` to defeat a global `pull.rebase=true` (see [[buildmesh-pull-rebase-default]], ADR 0001); **`git_sync` does not** — so the manual sync still carries the conflict-marker bug the auto-sync already fixed.
- **The Worktree Node has three homes.** Creating one lives in `env/mod.rs` (`create_git_worktree`), checking it's safe to close in `services/agent_node.rs` (`close_safety_for_worktree_path`), and removing it in `commands/prune.rs` (`remove_one_worktree`). A first-class domain object (CONTEXT.md: *Worktree Node*) with no module.

The mesh-health code added for issue #231 already demonstrates the target shape: pure `pub(crate)` functions take `&Repository` (`compute_mesh_health`, `restore_to_base_impl`, `find_base_branch_holder`, …) and the `#[command]` does only `to_host_path` + `Repository::open` + delegate. We are extending that one good pattern across the rest of the git surface.

## Decision

Introduce `src-tauri/src/git/`, built in stages so each lands as an independently-green commit:

1. **`git/primitives.rs`** — the shared low-level helpers, each taking `&Repository` (or an OID pair) and returning a `Result`/`Option` so **callers keep their own fail-open vs fail-closed choice**: `is_dirty`, `ahead_behind`, `short_sha`, `head_branch_name`, `open_from_host_path`, path-normalisation. All existing copies are deleted in favour of these. The one canonical dirty-check uses `!is_ignored()` (the `&& != Status::CURRENT` clause is redundant — `StatusOptions` never sets `include_unmodified`, so `CURRENT` entries are never emitted).
2. **`git/worktree.rs`** — the Worktree Node lifecycle: create (from `env/mod.rs`), close-safety (from `services/agent_node.rs`), remove + retry-rename staging (from `commands/prune.rs`). The *path* of a worktree (`resolve_agent_path`, the `.claude/worktrees/<name>` layout, host/spawn conversion) stays in `env` — `git` owns "make/inspect/remove the git worktree at this host path", `env` owns "where is that path". Queue/DB orchestration (`process_pending_removals`, the drain lock) stays in `services::agent_node`.
3. **`git/health.rs` + `git/sync.rs`** — mesh health/status/branch/prune-info reads, and the two sync entry points sharing one fetch→ff core (which fixes the missing `--no-rebase` in `git_sync`). With worktree-create and sync gone, `env/mod.rs` is reduced to environment detection and path conversion.

Throughout, `#[command]` functions remain in `commands/` as thin adapters (`to_host_path` + open + delegate), so the `lib.rs` handler list and all Tauri command names are untouched.

This explicitly **supersedes the inline "kept decoupled" rationale** at the three sites named above: the shared seam is now the `git` module, not duplicated private helpers.

## Considered alternatives

- **Leave it as-is (duplication with decoupling comments).** Rejected: the duplication has already diverged in both semantics and safety direction, and `git_sync` is missing a fix its twin has. The "decoupling" is illusory — everything is coupled to `git2`.
- **Extract only the primitives, stop there.** This is in fact stage 1; we keep going because the Worktree Node scattering (create/safety/remove across three modules) is the bigger AI-navigability and testability cost, and the health/sync moves are what let `env` become a clean path module.
- **Put it under `services/git/`.** Rejected in favour of a top-level `git/`, matching the repo's existing top-level domain modules (`env/`, `agent/`, `pty/`, `db/`, `http/`). `services/` here is the DB/orchestration layer; the git module is lower-level than that and is consumed by both `commands/` and `services/`.

## Consequences

- **One place owns git2.** The dirty-check, ahead/behind, short-sha, and head-branch logic have a single definition; the divergent-semantics bug class is gone. Callers choose their own error fallback at the call site, so the fail-closed (gates) vs fail-open (display) distinction is preserved deliberately rather than by accident.
- **The Worktree Node becomes testable as a unit** through one interface (create → close-safety → remove against a `TempGitRepo`), instead of only end-to-end through a real spawn.
- **`git_sync` gains `--no-rebase`** as a side effect of sharing the fetch core — a latent conflict-marker bug fixed for free.
- **`env/mod.rs` shrinks to its deep core** (path + environment conversion), so the WSL-path hard rule guards a smaller, single-purpose module.
- **Test fixtures move.** `env/mod.rs`'s `test_helpers` (shared by the worktree and fetch suites) relocate alongside the code under test; this is mechanical but touches ~700 lines of tests. Windows worktree cargo-test staleness applies (see [[buildmesh-cargo-test-incremental-staleness]]) — a `cargo clean -p` may be needed for moved `#[test]` fns to actually run.
- **ADR 0001 (auto-sync) and ADR 0003 (Buildmesh owns worktree creation) are unaffected in behaviour** — this is a relocation/deduplication of their *mechanism*, not a change to what they do.
- **Doc debt:** `docs/knowledge-primer.md` references `create_git_worktree`/`fetch_origin` at `env/mod.rs` paths that will move; those pointers must be updated when the code lands.
