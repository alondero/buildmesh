#![allow(unused_imports)]

use super::{prepare::*, provision::*, *};
use crate::git::worktree::provision::{
    adopt_warm_worktree_by_move, fetch_fork_head, fetch_single_ref, fork_remote_alias,
    locked_fetch_pr_head, read_origin_ref_sha, upgrade_warm_to_mode,
};
use tempfile::TempDir;

/// Atomic counter for unique bare-repo paths (one per test run).
static NEXT_FORK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Pin the spawn-time fallback. Sole pin of `DEFAULT_WORKTREE_MODE`
/// after #411 deleted the TS-side sentinel (it had no real consumer).
#[test]
fn default_worktree_mode_is_branched() {
    assert_eq!(DEFAULT_WORKTREE_MODE, "branched");
}

#[test]
fn provider_provisioning_runs_hooks_after_trust_failure() {
    let trust_finished = std::cell::Cell::new(false);
    let hook_saw_trust_finish = std::cell::Cell::new(false);
    let (trust, hooks) = run_provider_provisioning(
        || {
            trust_finished.set(true);
            Err("trust failed".to_string())
        },
        || {
            hook_saw_trust_finish.set(trust_finished.get());
            Err("hooks failed".to_string())
        },
        true,
    );

    assert_eq!(trust.unwrap_err(), "trust failed");
    assert_eq!(hooks.unwrap_err(), "hooks failed");
    assert!(hook_saw_trust_finish.get());

    let hook_called = std::cell::Cell::new(false);
    let (trust, hooks) = run_provider_provisioning(
        || Ok(()),
        || {
            hook_called.set(true);
            Ok(())
        },
        false,
    );
    assert!(trust.is_ok());
    assert!(hooks.is_ok());
    assert!(!hook_called.get());
}

// -----------------------------------------------------------------------
// Warm-pool manual claim — .worktreeinclude re-application (issue #639
// gap 1). The cold `create_git_worktree` and the Issue/PR `adopt…by_move`
// both call `apply_worktree_include` so an adopted worktree is byte-for-
// byte equivalent to a cold spawn. The manual warm-claim fast path
// (upgrade_warm_to_mode) MUST do the same — otherwise a user who edits a
// `.worktreeinclude`-referenced file (typical: `.env`, build cache) between
// prewarm time and spawn time lands on a stale copy.
// -----------------------------------------------------------------------

#[test]
fn upgrade_warm_to_mode_reapplies_worktreeinclude_after_checkout() {
    use std::fs;
    let (_td, root, pool) = setup_warm_pool_with_include();

    // User edits the source file BETWEEN prewarm and manual spawn —
    // exactly the window the missing apply_worktree_include used to leak.
    fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

    // The manual warm claim's mode upgrade — must re-copy `.worktreeinclude`
    // sources so the agent's worktree matches the live repo state, not the
    // stale prewarm snapshot.
    upgrade_warm_to_mode(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "bold-amber-fox",
        "branched",
    )
    .expect("upgrade_warm_to_mode must succeed");

    // The worktree's `.worktreeinclude`-referenced file must now reflect
    // the live repo content (NEW), not the prewarm-time snapshot (old).
    assert_eq!(
        fs::read_to_string(pool.join("secrets.env")).unwrap(),
        "v1=NEW\n",
        "manual warm claim must re-apply .worktreeinclude so the agent sees the live source"
    );
}

/// No `.worktreeinclude` at the repo root → the upgrade is still a no-op
/// rather than an error. Prevents a regression where adding the include
/// re-application broke a repo that never used the feature.
#[test]
fn upgrade_warm_to_mode_is_noop_when_no_worktreeinclude() {
    use crate::env::test_helpers::init_repo_with_commit;
    use std::fs;
    // Skip the .worktreeinclude side of the helper — bare repo + pool.
    let td = TempDir::new().unwrap();
    let root = td.path();
    let _ = init_repo_with_commit(root, &[("f.txt", "tracked\n")]);
    let pool = root
        .join(".claude")
        .join("worktrees")
        .join("warm-amber-fox");
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
        "HEAD",
    )
    .unwrap();
    let _ = td; // keep alive for the duration of the test

    upgrade_warm_to_mode(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "bold-amber-fox",
        "branched",
    )
    .expect("must succeed when no .worktreeinclude exists");
    // No spurious `.worktreeinclude` was created in the worktree.
    assert!(
        !pool.join(".worktreeinclude").exists(),
        "absent manifest must not be materialised by the upgrade"
    );
    // The tracked file round-trips.
    assert_eq!(fs::read_to_string(pool.join("f.txt")).unwrap(), "tracked\n");
}

/// Detached mode must also re-apply `.worktreeinclude` (issue #639 gap 1,
/// review finding). The original `upgrade_warm_to_mode` returned early on
/// `mode == "detached"` and skipped the include copy — a regression that
/// re-instated that early-return would pass `…_reapplies…_after_checkout`
/// (branched) but leave a detached-mode spawn on the stale prewarm
/// snapshot, defeating the gap-1 fix for half the meshes.
#[test]
fn upgrade_warm_to_mode_reapplies_worktreeinclude_in_detached_mode() {
    use std::fs;
    let (_td, root, pool) = setup_warm_pool_with_include();

    // User edits the source — same window as the branched-mode test.
    fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

    // Upgrade in DETACHED mode. The branch name is unused (no checkout),
    // but we pass the preassigned slug for consistency with the call site.
    upgrade_warm_to_mode(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
    )
    .expect("upgrade_warm_to_mode must succeed in detached mode");

    assert_eq!(
        fs::read_to_string(pool.join("secrets.env")).unwrap(),
        "v1=NEW\n",
        "manual warm claim in detached mode must also re-apply .worktreeinclude"
    );
    // And the worktree stayed detached — no branch was created.
    let wt = git2::Repository::open(&pool).unwrap();
    assert!(
        wt.head_detached().unwrap_or(false),
        "detached mode must leave the worktree detached"
    );
}

/// Shared setup for the two `upgrade_warm_to_mode` `.worktreeinclude`
/// re-application tests (#642.5). The third test
/// (`…_is_noop_when_no_worktreeinclude`) deliberately inlines its own
/// setup because the no-manifest case is the whole point of that test
/// — running it through the helper would materialise `secrets.env` and
/// `.worktreeinclude` in the worktree, defeating the no-op assertion.
///
/// The helper stands up: a tempdir holding a real git repo with
/// `secrets.env` + `.worktreeinclude` (both tracked), AND a pool-shaped
/// DETACHED worktree under `.claude/worktrees/warm-amber-fox` that has
/// already had the include copied at prewarm time (so the tests assert
/// the upgrade re-applies, not the original copy). Both the branched and
/// the detached call-site tests cut the pool as detached (the pool's
/// on-disk shape) — the difference between them is the
/// `upgrade_warm_to_mode` mode argument, not the helper's setup.
///
/// Returns `(tempdir, repo_root_path, pool_path)`. The tempdir is held
/// to keep the underlying directory alive for the duration of the test
/// — dropping it would delete the repo and break subsequent asserts.
fn setup_warm_pool_with_include() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    use crate::env::test_helpers::{commit_file, init_repo_with_commit};
    use std::fs;

    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();

    init_repo_with_commit(&root, &[("f.txt", "tracked\n")]);
    fs::write(root.join("secrets.env"), "v1=old\n").unwrap();
    fs::write(root.join(".worktreeinclude"), "secrets.env\n").unwrap();
    // Commit the manifest so `.worktreeinclude` is reachable for `git
    // worktree add`; the pool helper copies files relative to the repo
    // root regardless of whether the manifest itself is tracked, but
    // committing keeps the test setup close to a realistic repo.
    let repo = git2::Repository::open(&root).unwrap();
    commit_file(&repo, &root, ".worktreeinclude", "secrets.env\n");

    let pool = root
        .join(".claude")
        .join("worktrees")
        .join("warm-amber-fox");
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
        "HEAD",
    )
    .expect("prewarm-shape worktree must be creatable for this helper");
    assert_eq!(
        fs::read_to_string(pool.join("secrets.env")).unwrap(),
        "v1=old\n",
        "prewarm-time copy must reflect the original source"
    );
    (td, root, pool)
}

// -----------------------------------------------------------------------
// Warm-pool Issue/PR adoption (issue #612): move a detached pool worktree
// onto the node's target name and check it out to the resolved base SHA on
// its own branch. These pin the code-review fixes for two confirmed bugs:
// resolving `base_ref` → SHA (offline resilience), and using `-b` (NOT
// `-B`) so a re-spawn can never force-reset a branch carrying prior work.
// -----------------------------------------------------------------------

#[test]
fn adopt_warm_worktree_moves_and_branches_at_base_sha() {
    use crate::env::test_helpers::init_repo_with_commit;
    let td = TempDir::new().unwrap();
    let root = td.path();
    let repo = init_repo_with_commit(root, &[("f.txt", "a\n")]);
    let head = repo
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
        .to_string();

    // The pool's on-disk shape: a DETACHED worktree under a plain slug.
    let pool = root
        .join(".claude")
        .join("worktrees")
        .join("warm-amber-fox");
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
        "HEAD",
    )
    .unwrap();

    let target = root.join(".claude").join("worktrees").join("gh123-fix");
    adopt_warm_worktree_by_move(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        target.to_str().unwrap(),
        "gh123-fix",
        "branched",
        "HEAD",
    )
    .expect("adoption must succeed");

    assert!(!pool.exists(), "pool directory must be gone after the move");
    assert!(
        target.exists(),
        "target directory must exist after the move"
    );
    let wt = git2::Repository::open(&target).unwrap();
    assert_eq!(
        wt.head().unwrap().shorthand().unwrap(),
        "gh123-fix",
        "the adopted worktree must be on the node's own branch"
    );
    assert_eq!(
        wt.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string(),
        head,
        "the branch must sit at the resolved base SHA"
    );
}

#[test]
fn adopt_warm_worktree_refuses_to_clobber_an_existing_branch() {
    use crate::env::test_helpers::init_repo_with_commit;
    let td = TempDir::new().unwrap();
    let root = td.path();
    let repo = init_repo_with_commit(root, &[("f.txt", "a\n")]);
    // A pre-existing deterministic branch standing in for a prior spawn's
    // work. Force-resetting it (the old `-B` bug) would orphan its commits.
    let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("gh7-x", &head_commit, false).unwrap();

    let pool = root
        .join(".claude")
        .join("worktrees")
        .join("warm-amber-fox");
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        "warm-amber-fox",
        "detached",
        "HEAD",
    )
    .unwrap();

    let target = root.join(".claude").join("worktrees").join("gh7-x");
    let err = adopt_warm_worktree_by_move(
        root.to_str().unwrap(),
        pool.to_str().unwrap(),
        target.to_str().unwrap(),
        "gh7-x",
        "branched",
        "HEAD",
    )
    .expect_err("adoption must refuse to overwrite an existing branch");
    assert!(
        err.contains("already exists"),
        "the failure must name the existing branch refusal, got: {}",
        err
    );
    // Fail-fast contract: refusal is pre-move — see the guard in
    // `adopt_warm_worktree_by_move`.
    assert!(
        pool.exists(),
        "pool entry must be untouched after a refused adoption"
    );
    assert!(
        !target.exists(),
        "target must not be materialised by a refused adoption"
    );
}

// -----------------------------------------------------------------------
// base_ref resolution (master-trunk regression)
//
// Pre-fix, the spawn path hardcoded `"origin/main"` as the default
// `base_ref` when the `meshes.base_ref` DB column was `'origin/main'`
// (its COALESCE default) — meaning a master-trunk repo always hit
// `mesh-sync-warning` on every spawn (`fatal: couldn't find remote
// ref main`). These tests pin the resolution chain:
//
//   1. meshes.base_ref (BUT NOT the COALESCE default — that's
//      treated as "no config" so the detection chain runs)
//   2. refs/remotes/origin/HEAD read from the local repo
//   3. "origin/main" last resort
//
// The COALESCE-sentinel treatment is critical: the DB column is
// NOT NULL with default `'origin/main'`, so `Mesh.base_ref` is
// ALWAYS a non-empty `String` and `MeshRow.base_ref` is ALWAYS
// `Some(_)` — a naive `if let Some(b) = config_base_ref { return b }`
// would make the detection chain dead code in production. The
// `resolve_base_ref_treats_coalesce_sentinel_as_unset` test pins the
// production call path (`Some("origin/main")`).
// -----------------------------------------------------------------------

#[test]
fn resolve_base_ref_uses_config_value_when_set() {
    // The config wins even on a non-repo / non-master path — explicit
    // user intent overrides any auto-detection. Empty / whitespace
    // config falls through to the detection chain (regression guard
    // for an empty-string value slipping through the COALESCE).
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("origin/develop")),
        "origin/develop"
    );
    // Empty / whitespace strings are treated as "no config" so the
    // detection chain runs — mirrors the COALESCE-to-default contract
    // in the DB layer.
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("")),
        "origin/main",
        "empty config base_ref must fall through to detection, not propagate"
    );
    assert_eq!(
        resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("   ")),
        "origin/main",
        "whitespace-only config base_ref must fall through to detection"
    );
}

#[test]
fn resolve_base_ref_falls_back_to_origin_main_for_non_repo() {
    // Non-repo path with no config — must not panic. Last-resort
    // behaviour preserved: `get_default_branch` returns "main" on a
    // failed `Repository::open`, and we prefix it with "origin/".
    // The spawn path itself short-circuits to `RepoUnusable` so the
    // auto-sync result is non-blocking.
    let tmp = TempDir::new().unwrap();
    let resolved = resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), None);
    assert_eq!(resolved, "origin/main");
}

#[test]
fn resolve_base_ref_detects_master_via_origin_head() {
    // Headline regression test: a master-trunk repo with no
    // `base_ref` in mesh config must produce "origin/master", not
    // the legacy "origin/main". Pre-fix, this always returned
    // "origin/main" and the spawn emitted a `mesh-sync-warning` on
    // every node.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_master");
    let parent = td.path();
    // Create a working repo on whatever default branch git picks.
    // The local branch name doesn't matter — what matters is that
    // `refs/remotes/origin/HEAD` points at `refs/remotes/origin/master`.
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    // Build the symbolic ref that `get_default_branch` reads.
    repo.reference("refs/remotes/origin/master", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
        true,
        "test setup",
    )
    .unwrap();

    // Sanity: precondition for the test to be meaningful.
    let head_ref = repo
        .find_reference("refs/remotes/origin/HEAD")
        .unwrap()
        .symbolic_target()
        .unwrap()
        .to_string();
    assert_eq!(
        head_ref, "refs/remotes/origin/master",
        "precondition: origin/HEAD must point at refs/remotes/origin/master"
    );

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
    assert_eq!(
        resolved, "origin/master",
        "master-trunk repo with no base_ref in config must yield origin/master, \
             not the legacy hardcoded origin/main (this is the master-trunk regression)"
    );
}

#[test]
fn resolve_base_ref_detects_main_via_origin_head() {
    // Sanity pin: the existing main-trunk behaviour (a repo whose
    // origin/HEAD points at `main`) must still resolve to
    // "origin/main" after the fix. Guards against the master fix
    // accidentally regressing the main case.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_main");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        true,
        "test setup",
    )
    .unwrap();

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
    assert_eq!(
        resolved, "origin/main",
        "main-trunk repo must still resolve to origin/main (no regression)"
    );
}

#[test]
fn resolve_base_ref_treats_coalesce_sentinel_as_unset() {
    // The production call path: `meshes.base_ref` is a NOT NULL
    // column with a COALESCE default of `'origin/main'` (see
    // `db::MESH_COLUMNS`). A fresh mesh whose base_ref was never
    // explicitly set reads as `Some("origin/main")` from the DB →
    // `MeshRow.base_ref = Some("origin/main")` →
    // `config.as_ref().and_then(|c| c.base_ref.as_deref())` returns
    // `Some("origin/main")`. The helper MUST treat this sentinel as
    // "no config" and fall through to the detection chain, otherwise
    // a master-trunk repo's spawn still hits `mesh-sync-warning`.
    // The earlier `_detects_master_via_origin_head` test passes
    // `None` (which never reaches production); THIS test pins the
    // actual production contract.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_coalesce_master");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/master", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
        true,
        "test setup",
    )
    .unwrap();

    // Production-shaped input: COALESCE default from the DB.
    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), Some("origin/main"));
    assert_eq!(
        resolved, "origin/master",
        "the COALESCE default 'origin/main' from a fresh mesh's DB row \
             must be treated as 'no config' — fall through to origin/HEAD \
             detection. A master-trunk repo with an unconfigured mesh \
             produces origin/master, not origin/main. This is the actual \
             production contract; the test passing None never reaches \
             production."
    );
}

#[test]
fn resolve_base_ref_keeps_explicit_user_value_for_main_trunk() {
    // A user who LEGITIMATELY sets `base_ref = "origin/main"` (via
    // the 'Fresh' UI option) on a main-trunk repo must still get
    // "origin/main" back. The COALESCE-sentinel treatment must
    // apply to the *fresh* / *unconfigured* case, not penalize a
    // user who explicitly chose the same value. For a main-trunk
    // repo the auto-detect would return the same value, so this
    // test is mostly a documentation pin.
    use crate::env::test_helpers::TestDir;
    use git2;

    let td = TestDir::new("base_ref_explicit_main");
    let parent = td.path();
    crate::env::test_helpers::init_repo_with_commit(parent, &[("README.md", "v1\n")]);

    let repo = git2::Repository::open(parent).unwrap();
    let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", oid, true, "test setup")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        true,
        "test setup",
    )
    .unwrap();

    let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), Some("origin/main"));
    assert_eq!(
        resolved, "origin/main",
        "explicit user-set 'origin/main' on a main-trunk repo must resolve \
             to 'origin/main' (same as auto-detect — no behaviour change)"
    );
}

// -----------------------------------------------------------------------
// SHA-drift detection (issue #444)
//
// `read_origin_ref_sha` returns the local SHA at `origin/<head_ref>` so
// the spawn path can compare it to the user-pinned `source_pr_pinned_sha`
// and emit a `pr_sha_drift` warning on mismatch. The unit test creates
// the local ref directly via git2 (no real remote / fetch roundtrip) so
// the test is hermetic and fast.
// -----------------------------------------------------------------------

#[test]
fn read_origin_ref_sha_returns_local_sha_when_ref_exists() {
    let tmp = TempDir::new().unwrap();
    let repo = git2::Repository::init(tmp.path()).unwrap();
    // Create a real commit on a known branch — we need a tree OID the
    // commit can point at. `Repository::init` leaves the index empty
    // but write_tree() on an empty index still produces a valid tree.
    let tree_oid = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    let commit_oid = repo
        .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    // Manually create the remote-tracking ref the function reads. In
    // production this is what `git fetch origin -- <head_ref>` writes;
    // here we shortcut the network roundtrip to keep the test hermetic.
    let ref_name = "refs/remotes/origin/feat-x";
    repo.reference(ref_name, commit_oid, true, "test").unwrap();

    let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/feat-x");
    assert_eq!(
        sha.as_deref(),
        Some(commit_oid.to_string().as_str()),
        "read_origin_ref_sha must return the full 40-char SHA the ref points to"
    );
}

#[test]
fn read_origin_ref_sha_returns_none_for_missing_ref() {
    let tmp = TempDir::new().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    // No refs/remotes/origin/* exists; the function must return None
    // (the spawn path treats this as "skip drift check" rather than
    // failing — same fail-open semantics as `pr_head_unfetchable`).
    let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/nope");
    assert!(sha.is_none(), "missing ref must return None, not error");
}

#[test]
fn read_origin_ref_sha_returns_none_for_non_git_directory() {
    // A path that isn't a git repo at all — `git rev-parse` exits non-zero,
    // the helper must swallow that and return None rather than panicking.
    let tmp = TempDir::new().unwrap();
    let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/main");
    assert!(sha.is_none(), "non-repo path must return None, not error");
}

// ----- fork alias + fetch_fork_head (issue #443) ---------------------

/// `fork-<login>` is the human-readable alias used in `git remote -v` and
/// the worktree `base_ref` string. The `fork-` prefix keeps our entries
/// easy to spot in the remote list and trivial to clean up if we ever
/// need to. Pin the format so a future refactor that swaps the prefix
/// surfaces as a test failure rather than a silent rename in user
/// worktrees.
#[test]
fn fork_remote_alias_uses_fork_prefix() {
    assert_eq!(fork_remote_alias("alice"), "fork-alice");
    assert_eq!(fork_remote_alias("alondero"), "fork-alondero");
}

/// Build a bare "fork" repo (a real local clone target so the test
/// doesn't need a network round-trip) and a regular repo that will
/// register the fork as a remote. The fork has a single commit on
/// `main` plus a `feat/443-fork` branch so the fetch can target a
/// non-default ref. Returns `(local, fork_bare_dir, fork_path)` —
/// the caller holds the dirs for the duration of the test.
fn init_fork_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    // Source: a regular repo with a feature branch we can fetch.
    let src = TempDir::new().unwrap();
    let src_path = src.path().to_path_buf();
    let src_repo = git2::Repository::init(&src_path).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    std::fs::write(src_path.join("README.md"), "fork-source\n").unwrap();
    let mut index = src_repo.index().unwrap();
    index.add_path(std::path::Path::new("README.md")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = src_repo.find_tree(tree_oid).unwrap();
    let main_commit = src_repo
        .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
    // Branch off a feature branch.
    let feat_commit = src_repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "feat: fork-only commit",
            &tree,
            &[&src_repo.find_commit(main_commit).unwrap()],
        )
        .unwrap();
    let _ = tree;
    // `main_commit` is a `git2::Oid` (Copy) — no need to `drop` it; the
    // explicit `drop()` was a no-op flagged by clippy.
    let feat_commit = src_repo.find_commit(feat_commit).unwrap();
    src_repo
        .branch("feat/443-fork", &feat_commit, true)
        .unwrap();
    // Bare clone target (so the fork has no working tree, like a real
    // remote on GitHub — `git fetch` reads its objects directly).
    // Use a unique, path-safe name — avoid `{:?}` on the source path
    // (it produces `C:\...` with backslashes and quotes that don't
    // round-trip as a directory name on Windows).
    let bare_dir = std::env::temp_dir().join(format!(
        "buildmesh_fork_bare_{}_{}",
        std::process::id(),
        NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    ));
    let _ = std::fs::remove_dir_all(&bare_dir);
    let clone = git2::Repository::init_bare(&bare_dir).unwrap();
    let mut remote = clone.remote("origin", src_path.to_str().unwrap()).unwrap();
    remote
        .fetch(&["refs/heads/*:refs/heads/*"], None, None)
        .unwrap();
    // Local: a fresh repo with no remotes — this is what
    // `fetch_fork_head` will register the fork on.
    let local = TempDir::new().unwrap();
    git2::Repository::init(local.path()).unwrap();
    (local, bare_dir, src_path)
}

/// First-time registration: the fork is added as `fork-alice` and the
/// head ref is materialised. `fetch_fork_head` returns `true` and
/// the resulting `git ls-remote` shows the ref under the alias.
/// This is the end-to-end "fork spawn" path that issue #443 opens up.
#[test]
fn fetch_fork_head_registers_remote_and_fetches_ref() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();

    let ok = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(ok, "fetch_fork_head must succeed on a real bare repo");

    // Verify the alias + URL are registered.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("fork-alice remote must be registered");
    let url = remote.url().expect("remote URL must be set");
    assert_eq!(
        url, bare_dir_str,
        "remote URL must match the fork's clone URL"
    );

    // Verify the ref was fetched — it should be visible as
    // `fork-alice/feat/443-fork`.
    let reference = local_repo
        .find_reference("refs/remotes/fork-alice/feat/443-fork")
        .expect("fetched ref must be present under fork-alice/");
    assert!(
        reference.target().is_some(),
        "ref target must be a real OID"
    );
}

/// Idempotent: a second call on a repo that already has the remote
/// registered AND the right URL is a no-op. The user can spawn a
/// second agent on the same fork PR (e.g. after closing the first)
/// without `git remote add` failing. The function still returns
/// `true` because the fetch succeeds.
#[test]
fn fetch_fork_head_is_idempotent_on_repeat_call() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();

    let first = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(first, "first call must succeed");

    // Second call with the SAME URL — must not error (the `remote add`
    // path is the failure-prone one without the existence check; the
    // `get-url` probe should return the right URL and skip the add).
    let second = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bare_dir_str,
        "feat/443-fork",
    );
    assert!(second, "second call must still succeed (idempotent)");

    // Remote is still there, single entry.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("fork-alice remote must still be registered after repeat call");
    assert_eq!(remote.url().unwrap(), bare_dir_str);
}

/// URL drift: if the fork's clone URL changes between spawns (the
/// user renamed the repo, or — more likely — the first call stored a
/// stale URL), the second call should update the existing remote's
/// URL via `git remote set-url` rather than fail or keep the stale
/// URL. Pin this so a future refactor that skips the set-url branch
/// surfaces as a test failure (the second call would silently fetch
/// the wrong ref).
#[test]
fn fetch_fork_head_updates_url_on_drift() {
    let (local, bare_dir, _src) = init_fork_fixture();
    let stale_url = bare_dir.to_str().unwrap().to_string();
    // Reuse the SAME bare dir (so the second call still finds a real
    // repo) but pretend the URL "drifted" by passing a different
    // string that ALSO resolves to the same on-disk repo. We achieve
    // that with a file:// URL on Windows (path with backslashes
    // round-trip cleanly through git remote add).
    let drifted_url = format!("file://{}", stale_url.replace('\\', "/"));

    // First call: register the stale URL.
    let first = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &stale_url,
        "feat/443-fork",
    );
    assert!(first, "first call must succeed");

    // Second call: same alias, drifted URL — the function should run
    // `git remote set-url` and re-fetch.
    let second = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &drifted_url,
        "feat/443-fork",
    );
    assert!(second, "second call must still succeed after URL drift");

    // The stored URL must be the drifted one, not the original.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let remote = local_repo
        .find_remote("fork-alice")
        .expect("remote must still be registered");
    let stored = remote.url().unwrap();
    // git normalises file:// URLs slightly on Windows — assert it's
    // the drifted one rather than the original.
    assert_ne!(
        stored, stale_url,
        "URL must have been updated, not left at the stale value"
    );
}

/// Failure path: a non-existent clone URL must return `false` rather
/// than panic. The caller (`spawn_agent_inner`) falls back to the
/// mesh's `base_ref` and emits a `mesh-sync-warning` toast with
/// `outcome: "pr_fork_unfetchable"`. Without the failure-as-false
/// contract, a typo'd clone URL would either spawn on the wrong
/// commits silently or surface as a hard error every offline session.
#[test]
fn fetch_fork_head_returns_false_on_bad_clone_url() {
    let (local, _bare_dir, _src) = init_fork_fixture();
    let bad_url = "/nonexistent/path/to/fork/that/does/not/exist".to_string();

    let ok = fetch_fork_head(
        local.path().to_str().unwrap(),
        "alice",
        &bad_url,
        "feat/443-fork",
    );
    assert!(!ok, "fetch_fork_head must return false on a bad clone URL");
}

// ----- fetch_single_ref (issue #420) ---------------------------------
//
// Same-repo PR spawn (#420) — the worktree adoption path calls
// `fetch_single_ref` to materialise `origin/<head_ref>` so the worktree
// can be cut from it. As of issue #446 the function is a thin wrapper
// over `git::sync::do_fetch_only` (the fetch-only half of `do_sync` —
// open + dirty-check + has-remote + `git fetch`, NO `git pull` tail);
// the `-`-adversarial-ref hardening is preserved at the wrapper
// boundary because `do_fetch_only` passes the branch as a plain argv
// entry without a `--` separator (it doesn't know about the spawn
// context).
//
// These tests pin the cases the issue calls out:
//   1. success — ref exists on origin
//   2. ref-not-found — ref missing on origin (caller falls back to base_ref)
//   3. non-git path — caller passed a directory that isn't a repo
//   4. adversarial ref — `-`-prefixed input is rejected by the wrapper
//      before `do_fetch_only` sees it (the hardening migrated from the
//      shell-out's `--` separator to an upfront string check, since
//      `do_fetch_only` doesn't pass a `--` separator to `git fetch`)
//   5. dirty-skip (issue #446 acceptance #2) — a parent repo with
//      uncommitted changes must return `false` (mirrors
//      `fetch_origin_skips_dirty_parent` in `git/fetch_origin_tests.rs`)
//
// The fixture mirrors `init_fork_fixture` but for the same-repo path:
// a bare repo holds a single branch, the local repo has `origin`
// pointed at the bare, and the test calls `fetch_single_ref` against
// the local repo's path.

/// Build a "remote + local" pair: the bare repo has a single commit on
/// `main` plus a `feat/420-pr-spawn` branch; the local repo has `origin`
/// pointed at the bare. Returns `(local, bare_path)` — the local TempDir
/// owns its on-disk path; `bare_path` is a plain PathBuf that lives
/// inside `std::env::temp_dir()` and is reused across calls (it gets
/// re-populated with the same content each time, so the SHA is stable
/// per-test-process).
fn init_same_repo_fixture() -> (TempDir, std::path::PathBuf) {
    // Source: a working repo with a feature branch we can fetch.
    // We reuse the same on-disk source across tests in a single
    // process — `init_same_repo_fixture` is only called from the
    // same-repo tests below, and the contents are deterministic.
    static SRC_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let src_path = SRC_DIR
        .get_or_init(|| {
            let src = TempDir::new().unwrap();
            let src_path = src.path().to_path_buf();
            let src_repo = git2::Repository::init(&src_path).unwrap();
            let sig = git2::Signature::now("test", "test@example.com").unwrap();
            std::fs::write(src_path.join("README.md"), "init\n").unwrap();
            let mut index = src_repo.index().unwrap();
            index.add_path(std::path::Path::new("README.md")).unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = src_repo.find_tree(tree_oid).unwrap();
            let main_commit = src_repo
                .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
            let main_commit_obj = src_repo.find_commit(main_commit).unwrap();
            src_repo
                .branch("feat/420-pr-spawn", &main_commit_obj, true)
                .unwrap();
            // Leak the TempDir guard — we want src_path to stay alive
            // for the whole process, and the bare-fetch step below
            // re-reads from the on-disk path on every test.
            std::mem::forget(src);
            src_path
        })
        .clone();

    // Bare remote — same pattern as `init_fork_fixture`. A unique
    // name per process so parallel `cargo test` invocations don't
    // collide on the bare dir.
    let bare_dir = std::env::temp_dir().join(format!(
        "buildmesh_same_repo_bare_{}_{}",
        std::process::id(),
        NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    ));
    let _ = std::fs::remove_dir_all(&bare_dir);
    let clone = git2::Repository::init_bare(&bare_dir).unwrap();
    let mut remote = clone.remote("origin", src_path.to_str().unwrap()).unwrap();
    remote
        .fetch(&["refs/heads/*:refs/heads/*"], None, None)
        .unwrap();

    // Local repo with `origin` pointed at the bare. `fetch_single_ref`
    // will use this `origin` remote to materialise the ref.
    let local = TempDir::new().unwrap();
    let local_repo = git2::Repository::init(local.path()).unwrap();
    local_repo
        .remote("origin", bare_dir.to_str().unwrap())
        .unwrap();
    (local, bare_dir)
}

/// Success path: a ref that exists on `origin` is fetched into
/// `refs/remotes/origin/<head_ref>` and the function returns `true`.
/// This is the happy path the spawn-time worktree adoption relies on.
#[test]
fn fetch_single_ref_returns_true_when_ref_exists() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        ok,
        "fetch_single_ref must return true when the ref exists on origin"
    );
    // Verify the ref actually got materialised — a true return with no
    // visible ref would mean a silent no-op, which is a worse failure
    // mode than a hard error.
    let local_repo = git2::Repository::open(local.path()).unwrap();
    let reference = local_repo
        .find_reference("refs/remotes/origin/feat/420-pr-spawn")
        .expect("origin/feat/420-pr-spawn must be materialised after success");
    assert!(
        reference.target().is_some(),
        "fetched ref must point at a real OID, not be unborn"
    );
}

/// Ref-not-found path: a ref that does NOT exist on `origin` causes
/// `git fetch` to exit non-zero. The function returns `false` (not
/// an error) so the spawn path can fall back to the mesh's
/// `base_ref` — this is the ADR 0001 offline pattern, surface as
/// `pr_head_unfetchable` rather than failing the spawn.
#[test]
fn fetch_single_ref_returns_false_when_ref_missing() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "does-not-exist");
    assert!(
        !ok,
        "fetch_single_ref must return false when the ref is missing on origin \
             (caller falls back to base_ref per the offline-fallback contract)"
    );
}

/// Non-git path: a directory that isn't a git repo at all. `git fetch`
/// errors immediately; the function swallows that and returns `false`.
/// This is the "user has a partial / broken clone" edge case — the
/// spawn must not panic.
#[test]
fn fetch_single_ref_returns_false_for_non_git_directory() {
    let tmp = TempDir::new().unwrap();
    let ok = fetch_single_ref(tmp.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        !ok,
        "fetch_single_ref must return false (not panic) for a non-git path"
    );
}

/// Adversarial-ref pin (issue #420 hardening): a ref starting with `-`
/// (e.g. `--upload-pack=evil`) is rejected by `git` itself because of
/// the `--` separator before `head_ref`. Without the separator, `git`
/// would parse `--upload-pack=evil` as a flag and use it for the
/// fetch — a vector for arbitrary command execution on a malicious
/// server (CVE-2017-1000117 / CVE-2018-17456 class). The hardening
/// lives in `fetch_single_ref`; this test pins the contract so a
/// future refactor that drops the `--` separator fails the test
/// rather than silently re-introducing the vulnerability.
///
/// We pass a ref that, WITHOUT the separator, `git` would parse as a
/// flag (`--upload-pack=evil`) — `git fetch` will then error out on
/// "fatal: bad config name", proving the separator did its job. With
/// the separator, the value reaches the ref-spec parser as a
/// literal ref name (which still doesn't exist on origin, so the
/// call returns `false` either way — the contract is "the function
/// returns false rather than letting `--upload-pack` reach git").
#[test]
fn fetch_single_ref_rejects_adversarial_dash_ref() {
    let (local, _bare) = init_same_repo_fixture();
    let ok = fetch_single_ref(local.path().to_str().unwrap(), "--upload-pack=evil");
    assert!(
        !ok,
        "fetch_single_ref must return false for a ref starting with '-' \
             (the wrapper rejects it before do_sync sees it)"
    );
}

/// Dirty-parent pin (issue #446 acceptance #2, inverted 2026-07-17): a
/// parent repo with uncommitted changes must STILL fetch the PR head.
/// A `git fetch` never touches the working tree — the pre-2026-07-17
/// dirty-skip meant a mesh whose root checkout stayed dirty silently
/// fell back to `base_ref` on every PR spawn, cutting the worktree
/// from the wrong commits. Pin the new contract so a future refactor
/// that re-introduces a pre-fetch dirty gate fails this test.
///
/// `is_dirty` includes untracked files, so writing one to the freshly-
/// init'd local repo is enough to dirty it — no need to seed a tracked
/// file first.
#[test]
fn fetch_single_ref_fetches_despite_dirty_parent() {
    let (local, _bare) = init_same_repo_fixture();
    // Precondition: the fixture's local repo must start clean, then we
    // make it dirty with an untracked file.
    assert!(
        !crate::env::test_helpers::repo_is_dirty(local.path()),
        "precondition: freshly-init'd local repo must start clean"
    );
    std::fs::write(local.path().join("dirty-marker.txt"), "uncommitted\n").unwrap();
    assert!(
        crate::env::test_helpers::repo_is_dirty(local.path()),
        "precondition: writing an untracked file must dirty the repo"
    );

    let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
    assert!(
        ok,
        "fetch_single_ref must fetch on a dirty parent — a fetch never \
             touches the working tree, and skipping cut PR worktrees from \
             stale refs"
    );
    // The head ref must be materialised so the worktree can be cut
    // from it — the whole point of the fetch.
    let repo = git2::Repository::open(local.path()).unwrap();
    assert!(
        repo.find_reference("refs/remotes/origin/feat/420-pr-spawn")
            .is_ok(),
        "the fetch must materialise refs/remotes/origin/<head_ref>"
    );
    // And the dirty marker must be untouched.
    assert_eq!(
        std::fs::read_to_string(local.path().join("dirty-marker.txt")).unwrap(),
        "uncommitted\n"
    );
}

// -----------------------------------------------------------------------
// locked_fetch_pr_head — per-Mesh sync_lock wrap (issue #698)
//
// `locked_fetch_pr_head` must run inside `services::sync_lock::with_mesh_
// sync_lock` so two concurrent PR-spawns (or a PR-spawn racing the manual
// `git_sync` from #680 / the spawn-time `fetch_origin` from #652) can't
// collide on `.git/FETCH_HEAD` / `.git/refs/remotes/<remote>/<ref>.lock`.
// Without the wrap the losing fetch fails with "another git process" and
// the spawn silently lands on `base_ref` (the wrong commits).
//
// We test the wrap with a wall-clock bound (mirroring the #680
// `git_sync_serializes_via_per_mesh_sync_lock_gh680` shape in
// `commands/git_tests.rs`). The `with_mesh_sync_lock` unit tests in
// `services::sync_lock` prove the primitive itself serialises; this test
// proves THIS specific call site uses the SAME key the spawn path uses,
// which is the bug class #698 closes.
//
// Holder enters the per-mesh lock and announces entry via an AtomicUsize
// flag before sleeping. Main thread spin-waits on the flag (deterministic
// — no `thread::sleep` race), then times `locked_fetch_pr_head`. With the
// wrap, `locked_fetch_pr_head` blocks ~450 ms waiting for the holder;
// without, it runs concurrently with the holder and finishes in tens of ms.
// -----------------------------------------------------------------------

/// Regression test for issue #698 — `locked_fetch_pr_head` must acquire
/// the per-Mesh `with_mesh_sync_lock` keyed on the spawn's `node.path`,
/// matching what `spawn_agent_inner` calls `fetch_origin` with two steps
/// earlier. Without this wrap, concurrent PR-spawns on the same Mesh
/// (and a PR-spawn racing the manual `git_sync` button) race on
/// `.git/FETCH_HEAD` / `refs/remotes/<remote>/<ref>.lock` and the loser
/// silently falls back to `base_ref`.
///
/// Strategy: holder thread enters `with_mesh_sync_lock(&path_key, ...)`
/// and announces via an AtomicUsize flag, then sleeps. Main thread
/// spin-waits on the flag (deterministic — no `thread::sleep` race), then
/// times `locked_fetch_pr_head`. With the wrap, `locked_fetch_pr_head`
/// blocks waiting for the holder; without, it returns immediately while
/// the holder is still inside its critical section.
///
/// Why wall-clock (not `fetch_add`): the per-Mesh lock is correctly
/// implemented (issue #652 + `services::sync_lock` unit tests prove it),
/// so it *prevents* simultaneous critical-section entries — `max_concurrent
/// == 1` even on a working lock. The only signal that `locked_fetch_pr_head`
/// shares the same key is that it waits for the holder to release the lock.
///
/// The test uses the same-repo branch (passes `None, None` for fork
/// fields). The fork branch shares the same wrapper so the regression
/// coverage is sufficient with one call site — a #698 regression that
/// branched out of the wrapper entirely would fail this test and the
/// #443 fork tests would still pass on the unwrapped helper, surfacing
/// the gap.
#[test]
fn locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
    use std::time::{Duration, Instant};

    let (local, _bare) = init_same_repo_fixture();
    let path_key = local.path().to_string_lossy().into_owned();

    // Holder enters the per-mesh lock and announces entry via
    // `entered_flag` before sleeping. Spinning on the flag avoids the
    // `thread::sleep` race — CI jitter can't make `locked_fetch_pr_head`
    // sneak in first.
    let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
    let holder_path = path_key.clone();
    let entered_holder = std::sync::Arc::clone(&entered_flag);
    let holder = std::thread::spawn(move || {
        crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
            entered_holder.store(1, AOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
        });
    });

    // Spin-wait (bounded) for the holder to actually be inside the
    // critical section. Cap at 2 s so a hung holder surfaces as a
    // test panic, not a forever-wait.
    let deadline = Instant::now() + Duration::from_secs(2);
    while entered_flag.load(AOrdering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "holder thread never entered the per-mesh lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    let _ = locked_fetch_pr_head(&path_key, "feat/420-pr-spawn", None, None);
    let elapsed = start.elapsed();

    holder.join().unwrap();

    // With wrap: elapsed >= ~450 ms (`locked_fetch_pr_head` waited for
    // the holder). Without wrap: elapsed = tens of ms (the fetch ran
    // concurrently with the holder's sleep). Bound is 400 ms — leaves
    // 100 ms of slack for setup overhead and CI jitter on a busy box.
    assert!(
        elapsed >= Duration::from_millis(400),
        "locked_fetch_pr_head did not block on the per-mesh lock \
             (elapsed = {:?}); issue #698 wrap is missing — concurrent PR-spawn \
             and spawn-time fetch_origin (or manual git_sync from #680) would \
             race on .git/FETCH_HEAD and refs/remotes/<remote>/<ref>.lock",
        elapsed,
    );
}

/// Companion to `locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698`
/// — exercises the FORK branch (`Some/Some` → `fetch_fork_head`) of the
/// wrapper. The same-repo test alone leaves a CI blind spot: a #698
/// regression that bypassed the wrapper for fork PRs (e.g. an inlined
/// `fetch_fork_head` call in `spawn_agent_inner` to skip the remote-
/// config lock acquisition) would still pass the same-repo test and
/// every existing #443 fork unit test (those hit the bare helper
/// directly, no lock). This test closes the gap by hitting the fork
/// arm of the wrapper with the same wall-clock shape; its `git remote
/// add` then `git fetch` sequence MUST hold the lock for the holder's
/// 500 ms sleep.
#[test]
fn locked_fetch_pr_head_serializes_fork_branch_via_per_mesh_sync_lock_gh698() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
    use std::time::{Duration, Instant};

    let (local, bare_dir, _src) = init_fork_fixture();
    let bare_dir_str = bare_dir.to_str().unwrap().to_string();
    let path_key = local.path().to_string_lossy().into_owned();

    // Holder enters the per-mesh lock (same key as the wrapper) and
    // announces via `entered_flag` before sleeping.
    let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
    let holder_path = path_key.clone();
    let entered_holder = std::sync::Arc::clone(&entered_flag);
    let holder = std::thread::spawn(move || {
        crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
            entered_holder.store(1, AOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
        });
    });

    let deadline = Instant::now() + Duration::from_secs(2);
    while entered_flag.load(AOrdering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "holder thread never entered the per-mesh lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    let _ = locked_fetch_pr_head(
        &path_key,
        "feat/443-fork",
        Some("alice"),
        Some(&bare_dir_str),
    );
    let elapsed = start.elapsed();

    holder.join().unwrap();

    assert!(
        elapsed >= Duration::from_millis(400),
        "locked_fetch_pr_head (fork branch) did not block on the per-mesh \
             lock (elapsed = {:?}); issue #698 wrap is missing for the fork path \
             — concurrent fork-PR spawns would race on .git/FETCH_HEAD, \
             refs/remotes/fork-<login>/<ref>.lock, AND the git remote add/config \
             files that fetch_fork_head writes before its fetch",
        elapsed,
    );
}
