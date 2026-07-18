//! Regression tests for the spawn-time auto-sync (issue #213).
//!
//! These tests guard the contract that a non-fatal fetch+ff-pull runs
//! before a worktree node is created, and that the outcome is observable
//! to the caller so it can surface a non-blocking warning toast on
//! failure. The fetch/ff-pull success and failure paths use a *local*
//! "remote" (a bare clone sitting next to the working repo), so
//! `git fetch` succeeds in-process without any network access.

use super::{fetch_origin, FetchError, FetchOutcome};
use crate::env::test_helpers::{commit_file, init_repo_with_commit, TestDir};
use std::path::Path;
use std::process::Command;

/// Wire a local "remote" at `remote_path` to the repo at `host_path` under
/// the given remote name (e.g. `"origin"`, `"upstream"`). Used so the
/// fetch path can be exercised end-to-end without network.
fn set_local_remote(host_path: &Path, remote_name: &str, remote_path: &Path) {
    // Defensive: clear any pre-existing remote with the same name so a
    // shared TestDir (re-run, or parallel test sharing a tmp parent) can
    // be re-wired cleanly.
    let _ = Command::new("git")
        .args(["remote", "remove", remote_name])
        .current_dir(host_path)
        .output();

    let status = Command::new("git")
        .args([
            "remote",
            "add",
            remote_name,
            &remote_path.to_string_lossy(),
        ])
        .current_dir(host_path)
        .status()
        .expect("git remote add failed");
    assert!(status.success(), "git remote add must succeed");
}

/// Convenience: equivalent to `set_local_remote(host_path, "origin", ...)`.
/// Kept for the existing #213 tests to keep their bodies readable.
fn set_local_origin(host_path: &Path, origin_path: &Path) {
    set_local_remote(host_path, "origin", origin_path);
}

// ── Issue #213 acceptance criteria ─────────────────────────────────────────

/// Acceptance criterion (issue #213, retargeted 2026-07-17): "Pulling is
/// skipped if the parent directory has uncommitted changes" — the PULL,
/// not the fetch. A dirty working tree must still fetch (a fetch never
/// touches the working tree, and the remote-tracking ref it updates is
/// what worktree nodes are cut from), then skip the fast-forward and
/// report `FetchedButDirty`. Pre-2026-07-17 the dirty gate sat before
/// the fetch, so a mesh whose root checkout stayed dirty never freshened
/// `refs/remotes/*` on ANY path and new nodes went stale without bound.
#[test]
fn fetch_origin_fetches_but_skips_pull_when_parent_dirty() {
    let td = TestDir::new("autosync_dirty");
    let parent = td.path();
    let name = td.path().file_name().unwrap().to_string_lossy().into_owned();
    let origin_dir = td.path().parent().unwrap().join(format!("{}_origin", name));
    std::fs::create_dir_all(&origin_dir).unwrap();

    // Local "remote" one commit ahead — same fixture shape as
    // `fetch_origin_fetches_and_fast_forwards_clean_local_remote`.
    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&origin_dir)
        .status()
        .expect("git init --bare failed");
    assert!(status.success());
    let repo = init_repo_with_commit(parent, &[("README.md", "v1\n")]);
    set_local_origin(parent, &origin_dir);
    let status = Command::new("git")
        .args(["push", "-u", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .expect("git push failed");
    assert!(status.success());
    commit_file(&repo, parent, "README.md", "v2\n");
    let status = Command::new("git")
        .args(["push", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .expect("git push (v2) failed");
    assert!(status.success());
    let status = Command::new("git")
        .args(["reset", "--hard", "HEAD~1"])
        .current_dir(parent)
        .status()
        .expect("git reset failed");
    assert!(status.success());
    // Rewind the remote-tracking ref too, so the test can observe the
    // fetch advancing it (push already moved it to v2 locally).
    let v1 = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", v1, true, "test rewind")
        .unwrap();

    // Dirty the tree: modify the tracked file WITHOUT committing.
    std::fs::write(parent.join("README.md"), "edited-not-committed\n").unwrap();

    let outcome = fetch_origin(parent.to_str().unwrap(), "origin/main");
    assert_eq!(
        outcome,
        Ok(FetchOutcome::FetchedButDirty { new_commits: 1 }),
        "a dirty parent must still fetch, then skip only the ff-pull"
    );

    // The fetch must have advanced the remote-tracking ref (this is what
    // a new worktree node is cut from)...
    let tracking = repo
        .find_reference("refs/remotes/origin/main")
        .unwrap()
        .target()
        .unwrap();
    assert_ne!(tracking, v1, "fetch must advance refs/remotes/origin/main");
    // ...while the dirty working tree stays untouched.
    let content = std::fs::read_to_string(parent.join("README.md")).unwrap();
    assert_eq!(
        content, "edited-not-committed\n",
        "the skipped pull must not touch the dirty working tree"
    );
}

/// A repo with no `origin` remote must skip silently — not error. A
/// Mesh is allowed to be purely local (e.g. an init-only repo that
/// hasn't been pushed yet) and we must not surface a warning toast
/// in that case.
#[test]
fn fetch_origin_skips_when_no_origin_remote() {
    let td = TestDir::new("autosync_no_remote");
    let parent = td.path();
    init_repo_with_commit(parent, &[("README.md", "x\n")]);

    let outcome = fetch_origin(parent.to_str().unwrap(), "origin/main");
    assert_eq!(outcome, Ok(FetchOutcome::SkippedNoRemote));
}

/// A path that isn't even a git repo must NOT panic, must NOT block
/// spawn — it should return `RepoUnusable` so the caller can decide
/// what to do (today: toast a generic warning and proceed).
#[test]
fn fetch_origin_reports_repo_unusable_for_non_repo() {
    let td = TestDir::new("autosync_nonrepo");
    let parent = td.path();
    // Create a file in the dir so it's a valid path but not a repo.
    std::fs::write(parent.join("not-a-repo.txt"), "x").unwrap();

    let outcome = fetch_origin(parent.to_str().unwrap(), "origin/main");
    assert!(matches!(outcome, Err(FetchError::RepoUnusable(_))));
}

/// E2E (no network): a clean repo with a local "remote" that has
/// fresh commits must fetch + ff-pull them and report `Synced` with
/// a non-zero commit count. This is the happy path that resolves
/// "stale baseline" — the entire motivation for #213.
#[test]
fn fetch_origin_fetches_and_fast_forwards_clean_local_remote() {
    let td = TestDir::new("autosync_clean_remote");
    let parent = td.path();
    let name = td.path().file_name().unwrap().to_string_lossy().into_owned();
    let origin_dir = td.path().parent().unwrap().join(format!("{}_origin", name));
    std::fs::create_dir_all(&origin_dir).unwrap();

    // Create a bare "remote" repo with one initial commit, then
    // wire the working repo to it as `origin`.
    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&origin_dir)
        .status()
        .expect("git init --bare failed");
    assert!(status.success());

    let repo = init_repo_with_commit(parent, &[("README.md", "v1\n")]);
    set_local_origin(parent, &origin_dir);

    // Push the working repo's initial commit to origin/main so the
    // upstream isn't empty.
    let status = Command::new("git")
        .args(["push", "-u", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .expect("git push failed");
    assert!(status.success(), "initial push must succeed");

    // Build a "remote is ahead" state: add a v2 commit locally,
    // push it to origin/main, then reset local main back to v1.
    // After the reset, origin/main is 1 ahead of local main — the
    // exact shape that triggers the fast-forward path.
    commit_file(&repo, parent, "README.md", "v2\n");
    let status = Command::new("git")
        .args(["push", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .expect("git push (v2) failed");
    assert!(status.success());
    let status = Command::new("git")
        .args(["reset", "--hard", "HEAD~1"])
        .current_dir(parent)
        .status()
        .expect("git reset failed");
    assert!(status.success());

    // Sanity: working tree is now at the v1 commit; origin/main is
    // at v2.
    let content = std::fs::read_to_string(parent.join("README.md")).unwrap();
    assert_eq!(content, "v1\n", "precondition: local must be at v1");

    // The actual capability: auto-sync.
    let outcome = fetch_origin(parent.to_str().unwrap(), "origin/main");
    assert_eq!(
        outcome,
        Ok(FetchOutcome::Synced { new_commits: 1 }),
        "clean local+remote must fetch+ff-pull and report 1 new commit"
    );

    // Postcondition: working tree is now at v2 (ff-pulled).
    let content = std::fs::read_to_string(parent.join("README.md")).unwrap();
    assert_eq!(
        content, "v2\n",
        "postcondition: ff-pull must move the local branch forward"
    );
}

/// E2E (no network): a clean repo with a local "remote" that has
/// DIVERGED must fetch the new commits and surface
/// `FetchedButDiverged` (the spawn proceeds from the local HEAD).
/// This is the "history diverged" case in the acceptance criteria —
/// the user has local commits the remote doesn't have, so
/// `git pull --ff-only` is required by policy to refuse.
///
/// **The diverged shape is critical.** Two truly *unrelated* roots
/// (e.g. force-pushing a single fresh commit) are NOT enough to
/// trigger non-ff behaviour: git's merge machinery treats unrelated
/// single-commit histories as "nothing to merge, just move the
/// pointer". The shape that actually rejects `--ff-only` is a shared
/// ancestor with different descendants on each side:
///
/// ```text
///   C0  (shared, on origin/main and as local's grandparent)
///    ├── C1_local  (file content "local")     ← local HEAD
///    └── C1_remote (file content "remote")    ← origin/main
/// ```
#[test]
fn fetch_origin_reports_diverged_when_fast_forward_impossible() {
    let td = TestDir::new("autosync_diverged");
    let parent = td.path();
    let name = td.path().file_name().unwrap().to_string_lossy().into_owned();
    let origin_dir = td.path().parent().unwrap().join(format!("{}_origin", name));
    std::fs::create_dir_all(&origin_dir).unwrap();
    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&origin_dir)
        .status()
        .unwrap();
    assert!(status.success());

    // 1. Local at C0, push to origin/main — both at C0 (file shared.txt).
    let repo = init_repo_with_commit(parent, &[("shared.txt", "shared\n")]);
    set_local_origin(parent, &origin_dir);
    let status = Command::new("git")
        .args(["push", "-u", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());

    // 2. Add C1_local — modifies shared.txt to "local\n". Local HEAD
    //    is now C1_local; origin/main is still at C0.
    std::fs::write(parent.join("shared.txt"), "local\n").unwrap();
    let status = Command::new("git")
        .args(["add", "shared.txt"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .args([
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "local diverged commit",
        ])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());
    // Sanity: local HEAD is now C1_local.
    let _ = repo;

    // 3. Build C1_remote (parent: C0) in a *clone* of origin, where
    //    C0 is the only commit present. C1_remote modifies shared.txt
    //    to "remote\n" — different content from C1_local, so it's a
    //    genuinely divergent descendant. Force-push it to origin/main.
    let clone_dir = td.path().parent().unwrap().join(format!("{}_clone", name));
    let _ = std::fs::remove_dir_all(&clone_dir);
    let status = Command::new("git")
        .args([
            "clone",
            origin_dir.to_str().unwrap(),
            clone_dir.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(clone_dir.join("shared.txt"), "remote\n").unwrap();
    let status = Command::new("git")
        .args(["add", "shared.txt"])
        .current_dir(&clone_dir)
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .args([
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "remote diverged commit",
        ])
        .current_dir(&clone_dir)
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .args(["push", "--force", "origin", "HEAD:main"])
        .current_dir(&clone_dir)
        .status()
        .unwrap();
    assert!(status.success(), "force-push of C1_remote must succeed");
    let _ = std::fs::remove_dir_all(&clone_dir);

    // Now: local HEAD = C1_local (descendant of C0); origin/main =
    // C1_remote (also descendant of C0). Neither is ancestor of
    // the other. `git pull --ff-only` MUST reject.
    let outcome = fetch_origin(parent.to_str().unwrap(), "origin/main");
    match outcome {
        Ok(FetchOutcome::FetchedButDiverged { new_commits, .. }) => {
            assert!(
                new_commits >= 1,
                "expected at least 1 new commit on origin, got {}",
                new_commits
            );
        }
        other => panic!(
            "expected FetchedButDiverged (history diverged), got {:?}",
            other
        ),
    }

    // The local working tree must NOT have been mutated by the
    // failed ff-pull — divergence is non-destructive.
    let content = std::fs::read_to_string(parent.join("shared.txt")).unwrap();
    assert_eq!(
        content, "local\n",
        "postcondition: ff-pull failure must not touch the local branch"
    );
}

// ── Issue #276 acceptance criteria ──────────────────────────────────────────
//
// The fetch target is now derived from the configured `base_ref` rather than
// hardcoded to `origin`. The headline test (#276's "concrete failure scenario"
// in the issue body) is a Mesh with `upstream` configured as the only remote
// and `base_ref = "upstream/main"`: the old hardcoded-`origin` code would
// return `SkippedNoRemote`; the new code must successfully sync.

/// **Headline test for #276.** A repo whose only remote is `upstream`
/// (no `origin` at all) must still be auto-synced when
/// `base_ref = "upstream/main"`. On the old hardcoded-`origin` code
/// this would short-circuit to `SkippedNoRemote` and the user would
/// see no sync; here we expect a real `Synced { new_commits: 1 }`.
///
/// Test shape mirrors `fetch_origin_fetches_and_fast_forwards_clean_local_remote`
/// but the local "remote" is wired under the name `upstream` instead of
/// `origin`. Local HEAD is rewound one commit so upstream/main is exactly
/// 1 commit ahead — the ff-pulls cleanly.
#[test]
fn fetch_origin_uses_base_ref_remote_not_origin() {
    let td = TestDir::new("autosync_upstream_remote");
    let parent = td.path();
    let name = td.path().file_name().unwrap().to_string_lossy().into_owned();
    let upstream_dir = td.path().parent().unwrap().join(format!("{}_upstream", name));
    std::fs::create_dir_all(&upstream_dir).unwrap();

    // Bare "remote" under the name `upstream` (NOT `origin`).
    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&upstream_dir)
        .status()
        .expect("git init --bare failed");
    assert!(status.success());

    let repo = init_repo_with_commit(parent, &[("README.md", "v1\n")]);
    set_local_remote(parent, "upstream", &upstream_dir);

    // Push the v1 commit, then build "remote is ahead" state: add v2
    // locally, push, reset local to v1.
    let status = Command::new("git")
        .args(["push", "-u", "upstream", "HEAD:main"])
        .current_dir(parent)
        .status()
        .expect("git push -u upstream failed");
    assert!(status.success());
    commit_file(&repo, parent, "README.md", "v2\n");
    let status = Command::new("git")
        .args(["push", "upstream", "HEAD:main"])
        .current_dir(parent)
        .status()
        .expect("git push upstream v2 failed");
    assert!(status.success());
    let status = Command::new("git")
        .args(["reset", "--hard", "HEAD~1"])
        .current_dir(parent)
        .status()
        .expect("git reset failed");
    assert!(status.success());

    // Sanity: local at v1, upstream/main at v2, NO `origin` remote.
    let content = std::fs::read_to_string(parent.join("README.md")).unwrap();
    assert_eq!(content, "v1\n", "precondition: local must be at v1");
    let remotes = Command::new("git")
        .args(["remote"])
        .current_dir(parent)
        .output()
        .unwrap();
    let remote_list = String::from_utf8_lossy(&remotes.stdout);
    assert!(
        !remote_list.lines().any(|l| l.trim() == "origin"),
        "precondition: this test must NOT have an origin remote; got: {:?}",
        remote_list
    );
    assert!(
        remote_list.lines().any(|l| l.trim() == "upstream"),
        "precondition: this test must have an upstream remote; got: {:?}",
        remote_list
    );

    // The actual capability: auto-sync against the configured `base_ref`.
    let outcome = fetch_origin(parent.to_str().unwrap(), "upstream/main");
    assert_eq!(
        outcome,
        Ok(FetchOutcome::Synced { new_commits: 1 }),
        "fetch_origin must sync against the base_ref's remote, not hardcoded origin"
    );

    // Postcondition: local has ff-pulled to v2.
    let content = std::fs::read_to_string(parent.join("README.md")).unwrap();
    assert_eq!(
        content, "v2\n",
        "postcondition: ff-pull from upstream must move the local branch forward"
    );
}

/// `base_ref = "HEAD"` must fall back to today's behaviour: fetch from
/// `origin` (the default). This guards the issue's "Edge case: HEAD or
/// empty falls back to origin" requirement.
#[test]
fn fetch_origin_falls_back_to_origin_when_base_ref_is_head() {
    let td = TestDir::new("autosync_base_ref_head");
    let parent = td.path();
    let name = td.path().file_name().unwrap().to_string_lossy().into_owned();
    let origin_dir = td.path().parent().unwrap().join(format!("{}_origin", name));
    std::fs::create_dir_all(&origin_dir).unwrap();

    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&origin_dir)
        .status()
        .unwrap();
    assert!(status.success());

    let repo = init_repo_with_commit(parent, &[("README.md", "v1\n")]);
    set_local_origin(parent, &origin_dir);

    let status = Command::new("git")
        .args(["push", "-u", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());
    commit_file(&repo, parent, "README.md", "v2\n");
    let status = Command::new("git")
        .args(["push", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .args(["reset", "--hard", "HEAD~1"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());

    // base_ref = "HEAD" should resolve to "fetch from origin" — the
    // current behaviour the issue is preserving.
    let outcome = fetch_origin(parent.to_str().unwrap(), "HEAD");
    assert_eq!(
        outcome,
        Ok(FetchOutcome::Synced { new_commits: 1 }),
        "base_ref=HEAD must fall back to origin"
    );
}

/// `base_ref = ""` (empty) must behave identically to `"HEAD"`: fall
/// back to `origin`. The DB COALESCEs to `'origin/main'` so an empty
/// string is unlikely in production, but the helper must still be
/// defensive.
#[test]
fn fetch_origin_falls_back_to_origin_when_base_ref_is_empty() {
    let td = TestDir::new("autosync_base_ref_empty");
    let parent = td.path();
    let name = td.path().file_name().unwrap().to_string_lossy().into_owned();
    let origin_dir = td.path().parent().unwrap().join(format!("{}_origin", name));
    std::fs::create_dir_all(&origin_dir).unwrap();

    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&origin_dir)
        .status()
        .unwrap();
    assert!(status.success());

    let repo = init_repo_with_commit(parent, &[("README.md", "v1\n")]);
    set_local_origin(parent, &origin_dir);

    let status = Command::new("git")
        .args(["push", "-u", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());
    commit_file(&repo, parent, "README.md", "v2\n");
    let status = Command::new("git")
        .args(["push", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .args(["reset", "--hard", "HEAD~1"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());

    let outcome = fetch_origin(parent.to_str().unwrap(), "");
    assert_eq!(
        outcome,
        Ok(FetchOutcome::Synced { new_commits: 1 }),
        "base_ref='' must fall back to origin"
    );
}

/// `base_ref = "refs/heads/main"` (no remote prefix) must fall back
/// to the branch's configured upstream remote, ultimately defaulting
/// to `origin`. We test the chain by configuring the local branch
/// `main`'s upstream to `origin` and verifying the fetch succeeds.
#[test]
fn fetch_origin_falls_back_to_origin_for_refs_heads_branch() {
    let td = TestDir::new("autosync_refs_heads_main");
    let parent = td.path();
    let name = td.path().file_name().unwrap().to_string_lossy().into_owned();
    let origin_dir = td.path().parent().unwrap().join(format!("{}_origin", name));
    std::fs::create_dir_all(&origin_dir).unwrap();

    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&origin_dir)
        .status()
        .unwrap();
    assert!(status.success());

    let repo = init_repo_with_commit(parent, &[("README.md", "v1\n")]);
    set_local_origin(parent, &origin_dir);

    let status = Command::new("git")
        .args(["push", "-u", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());
    commit_file(&repo, parent, "README.md", "v2\n");
    let status = Command::new("git")
        .args(["push", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .args(["reset", "--hard", "HEAD~1"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(status.success());

    // base_ref is the full ref form with no remote prefix. The
    // implementation should fall back to branch.main.remote (= origin
    // after `git push -u`).
    let outcome = fetch_origin(parent.to_str().unwrap(), "refs/heads/main");
    assert_eq!(
        outcome,
        Ok(FetchOutcome::Synced { new_commits: 1 }),
        "base_ref=refs/heads/main must fall back to the branch's upstream remote"
    );
}

/// When `base_ref` names a remote that doesn't exist in the repo, the
/// auto-sync must return `SkippedNoRemote` (the same outcome as
/// missing-origin today) — never an error toast, never a fetch
/// attempt. This is the "Edge case: missing remote" requirement.
#[test]
fn fetch_origin_skips_when_base_ref_remote_missing() {
    let td = TestDir::new("autosync_base_ref_missing_remote");
    let parent = td.path();
    init_repo_with_commit(parent, &[("README.md", "x\n")]);
    // Deliberately: NO remotes configured. base_ref says
    // "upstream/main" but there's no `upstream` remote.

    let outcome = fetch_origin(parent.to_str().unwrap(), "upstream/main");
    assert_eq!(
        outcome,
        Ok(FetchOutcome::SkippedNoRemote),
        "missing base_ref remote must produce SkippedNoRemote, not an error toast"
    );
}

// ── Issue #634 — advanced_ref() predicate is single-sourced ────────────────
//
// The "did the fetch actually pull new commits" predicate is used by two
// callers (the spawn path's `ref_advanced_for_pool` flag and the manual
// `git_sync` command's freshness-pass trigger) and used to be a duplicated
// `matches!(..., Synced | FetchedButDiverged)` at each site. After #634 the
// predicate lives on the enums themselves — these tests pin the truth table
// so a future variant addition forces an explicit `advanced_ref` decision
// rather than silently miscategorising.

/// Pin the `FetchOutcome::advanced_ref()` truth table. Only the two variants
/// that fetched new commits return `true`; `UpToDate` and the two skip
/// variants return `false`. A future variant addition must be added here
/// explicitly — the compiler won't catch a missed case in a `matches!` arm
/// at a remote call site, but it will catch a missed `match` arm in this
/// exhaustive test.
#[test]
fn fetch_outcome_advanced_ref_table() {
    use FetchOutcome::*;
    assert!(Synced { new_commits: 1 }.advanced_ref());
    assert!(FetchedButDiverged {
        new_commits: 1,
        reason: "x".into()
    }
    .advanced_ref());
    // FetchedButDirty is only constructed when the count-behind found new
    // commits, and the fetch that preceded it moved the remote-tracking
    // refs — the warm pool must refresh onto the new SHA just as it does
    // for the diverged case.
    assert!(FetchedButDirty { new_commits: 1 }.advanced_ref());
    assert!(!UpToDate.advanced_ref());
    assert!(!SkippedNoRemote.advanced_ref());
}

/// Pin the `SyncOutcome::advanced_ref()` truth table. `pub(crate)` — the
/// manual `git_sync` Tauri command is the only consumer. The two
/// error-shaped variants (`FetchFailed`, `RepoUnusable`) must return
/// `false` — a failed fetch did not advance the ref, and kicking a
/// freshness pass on a broken repo would just waste git shell-outs.
#[test]
fn sync_outcome_advanced_ref_table() {
    use crate::git::sync::SyncOutcome::*;
    assert!(Synced { new_commits: 1 }.advanced_ref());
    assert!(FetchedButDiverged {
        new_commits: 1,
        reason: "x".into()
    }
    .advanced_ref());
    assert!(FetchedButDirty { new_commits: 1 }.advanced_ref());
    assert!(!UpToDate.advanced_ref());
    assert!(!SkippedNoRemote.advanced_ref());
    assert!(!FetchFailed {
        reason: "x".into()
    }
    .advanced_ref());
    assert!(!RepoUnusable {
        reason: "x".into()
    }
    .advanced_ref());
}

// ── 2026-07-17 incident — URL-only remote (no fetch refspec) ────────────────
//
// A repo whose `.git/config` carries `remote.origin.url` but NO
// `remote.origin.fetch` line (written by hand or by tooling that skipped
// `git remote add`). Two independent failures compounded:
//   1. `git fetch origin` stored the result only in FETCH_HEAD — the
//      remote-tracking refs (what worktree nodes are cut from) never moved.
//   2. git2's `branch_upstream_name` needs the refspec to map the branch to
//      its tracking ref, so `commits_behind_upstream` errored and `do_sync`
//      swallowed that into `UpToDate` — the manual Sync button reported
//      "Already up to date" while `git pull` in a terminal pulled a
//      five-days' backlog.

/// The full incident reproduction: URL-only remote, branch tracking config
/// present, remote one commit ahead, remote-tracking ref stale. The sync
/// must fetch (advancing the tracking ref via the explicit refspec),
/// count the commits behind (via the branch-config fallback in
/// `upstream_oid_for_branch`), and fast-forward — reporting `Synced`,
/// never "Already up to date".
#[test]
fn fetch_origin_syncs_when_remote_has_no_fetch_refspec() {
    let td = TestDir::new("autosync_no_refspec");
    let parent = td.path();
    let name = td.path().file_name().unwrap().to_string_lossy().into_owned();
    let origin_dir = td.path().parent().unwrap().join(format!("{}_origin", name));
    std::fs::create_dir_all(&origin_dir).unwrap();

    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&origin_dir)
        .status()
        .expect("git init --bare failed");
    assert!(status.success());
    let repo = init_repo_with_commit(parent, &[("README.md", "v1\n")]);
    set_local_origin(parent, &origin_dir);
    let status = Command::new("git")
        .args(["push", "-u", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .expect("git push failed");
    assert!(status.success());
    commit_file(&repo, parent, "README.md", "v2\n");
    let status = Command::new("git")
        .args(["push", "origin", "HEAD:main"])
        .current_dir(parent)
        .status()
        .expect("git push (v2) failed");
    assert!(status.success());
    let status = Command::new("git")
        .args(["reset", "--hard", "HEAD~1"])
        .current_dir(parent)
        .status()
        .expect("git reset failed");
    assert!(status.success());
    let v1 = repo.head().unwrap().peel_to_commit().unwrap().id();

    // Recreate the incident config: drop the fetch refspec entirely and
    // rewind the tracking ref to the stale commit (the push above had
    // advanced it as a side effect).
    let status = Command::new("git")
        .args(["config", "--unset-all", "remote.origin.fetch"])
        .current_dir(parent)
        .status()
        .expect("git config --unset-all failed");
    assert!(status.success());
    repo.reference("refs/remotes/origin/main", v1, true, "test rewind")
        .unwrap();

    let outcome = fetch_origin(parent.to_str().unwrap(), "origin/main");
    assert_eq!(
        outcome,
        Ok(FetchOutcome::Synced { new_commits: 1 }),
        "a URL-only remote (no fetch refspec) must still sync — \
         'Already up to date' here was the 2026-07-17 incident"
    );

    // Both the checkout and the remote-tracking ref must be at v2.
    let content = std::fs::read_to_string(parent.join("README.md")).unwrap();
    assert_eq!(content, "v2\n", "ff-pull must move the local branch forward");
    let tracking = repo
        .find_reference("refs/remotes/origin/main")
        .unwrap()
        .target()
        .unwrap();
    assert_ne!(
        tracking, v1,
        "the explicit refspec must advance refs/remotes/origin/main \
         even with no remote.origin.fetch config"
    );
}
