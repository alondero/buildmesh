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

/// Set `origin` to the given path on the repo at `path`. Used to wire
/// the working repo to a local "remote" so we can exercise the fetch
/// path without network.
fn set_local_origin(host_path: &Path, origin_path: &Path) {
    let _ = Command::new("git")
        .args(["remote", "remove", "origin"])
        .current_dir(host_path)
        .output();

    let status = Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            &origin_path.to_string_lossy(),
        ])
        .current_dir(host_path)
        .status()
        .expect("git remote add failed");
    assert!(status.success(), "git remote add must succeed");
}

// ── Issue #213 acceptance criteria ─────────────────────────────────────────

/// Acceptance criterion: "Pulling is skipped if the parent directory
/// has uncommitted changes." A repo with a dirty working tree must
/// report `SkippedDirty` and must NOT shell out to git at all.
#[test]
fn fetch_origin_skips_dirty_parent() {
    let td = TestDir::new("autosync_dirty");
    let parent = td.path();
    let _repo = init_repo_with_commit(parent, &[("README.md", "clean\n")]);
    // Modify the tracked file WITHOUT committing — that's what
    // makes the working tree dirty. (A `commit_file` would ADVANCE
    // HEAD and leave the tree clean, which would defeat the test.)
    std::fs::write(parent.join("README.md"), "edited-not-committed\n").unwrap();
    let repo = git2::Repository::open(parent).unwrap();
    let status = repo.statuses(None).unwrap();
    assert!(
        status.iter().any(|e| !e.status().is_ignored()),
        "precondition: parent must be dirty"
    );

    let outcome = fetch_origin(parent.to_str().unwrap());
    assert_eq!(outcome, Ok(FetchOutcome::SkippedDirty));
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

    let outcome = fetch_origin(parent.to_str().unwrap());
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

    let outcome = fetch_origin(parent.to_str().unwrap());
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
    let outcome = fetch_origin(parent.to_str().unwrap());
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
    let outcome = fetch_origin(parent.to_str().unwrap());
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
