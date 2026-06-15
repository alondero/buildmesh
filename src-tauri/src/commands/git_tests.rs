//! Tests for git operations
//!
//! These tests create real temporary git repositories to verify
//! the status-counting logic correctly counts added/modified/deleted files.
//!
//! Run with: cd src-tauri && cargo test git::tests

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    /// Creates a unique temp directory and returns its path.
    /// RAII guard deletes the directory on drop.
    struct TempGitRepo(std::path::PathBuf);

    impl TempGitRepo {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            let tmp = std::env::temp_dir().join(format!("buildmesh_git_test_{}", id));
            Self(tmp)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempGitRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Creates a temp git repo with an initial commit on "main"
    fn init_git_repo(path: &Path) -> git2::Repository {
        fs::create_dir_all(path).unwrap();
        let repo = git2::Repository::init(path).unwrap();

        let sig = git2::Signature::now("test", "test@example.com").unwrap();

        if repo.head().is_err() {
            // Write a file and stage it using add_path with a RELATIVE path
            let file_name = "file.txt";
            fs::write(path.join(file_name), "initial content").unwrap();

            let mut index = repo.index().unwrap();
            index.add_path(Path::new(file_name)).unwrap();
            index.write().unwrap();

            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
                .unwrap();
        }
        repo
    }

    /// Stage a file by name (relative to repo root) — writes content to disk and stages it
    fn stage_file(repo: &git2::Repository, repo_path: &Path, file_name: &str, content: &str) {
        let full_path = repo_path.join(file_name);
        fs::create_dir_all(full_path.parent().unwrap()).unwrap();
        fs::write(&full_path, content).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(Path::new(file_name)).unwrap();
        index.write().unwrap();
    }

    /// Commit all staged changes
    fn commit_staged(repo: &git2::Repository, message: &str) {
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        let tree_oid = repo.index().unwrap().write_tree().unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            message,
            &repo.find_tree(tree_oid).unwrap(),
            &[&parent],
        )
        .unwrap();
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));

        assert!(
            output.status.success(),
            "git {:?} failed\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_git_without_dir(args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));

        assert!(
            output.status.success(),
            "git {:?} failed\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Mirrors the status-counting logic from get_git_summary.
    /// Keep in sync with git.rs.
    fn count_status(repo: &git2::Repository) -> (usize, usize, usize, usize) {
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);

        let statuses = repo.statuses(Some(&mut opts)).unwrap();

        let mut total = 0usize;
        let mut added = 0usize;
        let mut modified = 0usize;
        let mut deleted = 0usize;

        for entry in statuses.iter() {
            let status_flag = entry.status();
            if status_flag.is_ignored() {
                continue;
            }
            total += 1;
            if status_flag.is_index_new() || status_flag.is_wt_new() {
                added += 1;
            } else if status_flag.is_index_modified() || status_flag.is_wt_modified() {
                modified += 1;
            } else if status_flag.is_index_deleted() || status_flag.is_wt_deleted() {
                deleted += 1;
            }
            // renamed and untracked contribute to total but not to specific buckets
        }

        (total, added, modified, deleted)
    }

    // ─── Tests ────────────────────────────────────────────────────────────────

    #[test]
    fn test_count_status_empty_repo() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());
        let (total, added, modified, deleted) = count_status(&repo);
        assert_eq!(total, 0);
        assert_eq!(added, 0);
        assert_eq!(modified, 0);
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_count_status_single_untracked_file() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // Untracked file — not staged, not committed
        fs::write(_repo.path().join("newfile.txt"), "hello").unwrap();

        let (total, added, modified, deleted) = count_status(&repo);
        assert_eq!(total, 1);
        assert_eq!(added, 1); // untracked counts as "added"
        assert_eq!(modified, 0);
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_count_status_staged_file() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // New file, staged (in index but not committed)
        stage_file(&repo, _repo.path(), "staged.txt", "staged content");

        let (total, added, modified, deleted) = count_status(&repo);
        assert_eq!(total, 1);
        assert_eq!(added, 1); // staged = index_new = "added"
        assert_eq!(modified, 0);
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_count_status_modified_file() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // Create, commit, then modify working-tree (not staged)
        stage_file(&repo, _repo.path(), "modified.txt", "original");
        commit_staged(&repo, "add modified.txt");

        // Modify the working tree copy (not staged)
        fs::write(_repo.path().join("modified.txt"), "modified content").unwrap();

        let (total, added, modified, deleted) = count_status(&repo);
        assert_eq!(total, 1);
        assert_eq!(added, 0);
        assert_eq!(modified, 1);
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_count_status_deleted_file() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // Create, commit, then delete
        stage_file(&repo, _repo.path(), "deleted.txt", "to be deleted");
        commit_staged(&repo, "add deleted.txt");

        // Delete from working tree
        fs::remove_file(_repo.path().join("deleted.txt")).unwrap();

        let (total, added, modified, deleted) = count_status(&repo);
        assert_eq!(total, 1);
        assert_eq!(added, 0);
        assert_eq!(modified, 0);
        assert_eq!(deleted, 1);
    }

    #[test]
    fn test_count_status_mixed_changes() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // 1. Untracked file (added)
        fs::write(_repo.path().join("untracked.txt"), "untracked").unwrap();

        // 2. Staged file (added)
        stage_file(&repo, _repo.path(), "staged.txt", "staged");

        // 3. Committed then modified (modified)
        stage_file(&repo, _repo.path(), "modified.txt", "original");
        commit_staged(&repo, "add modified.txt");
        fs::write(_repo.path().join("modified.txt"), "modified").unwrap();

        // 4. Committed then deleted (deleted)
        stage_file(&repo, _repo.path(), "deleted.txt", "to delete");
        commit_staged(&repo, "add deleted.txt");
        fs::remove_file(_repo.path().join("deleted.txt")).unwrap();

        let (total, added, modified, deleted) = count_status(&repo);

        // Expected: deleted.txt(wt_del), modified.txt(wt_mod), untracked.txt(wt_new) = 3
        // staged.txt: content matches disk, so git2 doesn't report it as changed.
        assert_eq!(total, 3, "expected 3 (untracked + modified + deleted — staged.txt content matches disk)");
        assert_eq!(added, 1, "expected 1 added (untracked.txt)");
        assert_eq!(modified, 1, "expected 1 modified");
        assert_eq!(deleted, 1, "expected 1 deleted");
    }

    #[test]
    fn test_count_status_ignored_file_not_counted() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // Commit a gitignore so git knows to respect it
        fs::write(_repo.path().join(".gitignore"), "ignored.txt\n").unwrap();
        stage_file(&repo, _repo.path(), ".gitignore", "ignored.txt\n");
        commit_staged(&repo, "add gitignore");

        // Now create ignored.txt — it should be ignored
        fs::write(_repo.path().join("ignored.txt"), "should be ignored").unwrap();

        let (total, _, _, _) = count_status(&repo);
        assert_eq!(total, 0, "ignored files should not appear in count");
    }

    #[test]
    fn test_count_status_renamed_file() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // Commit a file
        stage_file(&repo, _repo.path(), "oldname.txt", "content");
        commit_staged(&repo, "add oldname.txt");

        // Simulate a rename by removing old and adding new with same content
        fs::remove_file(_repo.path().join("oldname.txt")).unwrap();
        stage_file(&repo, _repo.path(), "newname.txt", "content");

        // Renamed files appear as deleted + added (git2 reports them as such)
        let (total, added, _modified, deleted) = count_status(&repo);
        assert_eq!(total, 2, "rename = deleted + added");
        assert_eq!(added, 1);
        assert_eq!(deleted, 1);
    }

    #[tokio::test]
    async fn git_sync_fetches_current_branch_upstream_when_remote_is_not_origin() {
        let remote = TempGitRepo::new();
        fs::create_dir_all(remote.path()).unwrap();
        git2::Repository::init_bare(remote.path()).unwrap();

        let seed = TempGitRepo::new();
        let seed_repo = init_git_repo(seed.path());
        run_git(seed.path(), &["branch", "-M", "main"]);
        run_git(seed.path(), &["remote", "add", "main", remote.path().to_str().unwrap()]);
        run_git(seed.path(), &["push", "-u", "main", "main"]);

        let local = TempGitRepo::new();
        run_git_without_dir(&[
            "clone",
            "-o",
            "main",
            "-b",
            "main",
            remote.path().to_str().unwrap(),
            local.path().to_str().unwrap(),
        ]);

        stage_file(&seed_repo, seed.path(), "remote-change.txt", "remote change");
        commit_staged(&seed_repo, "add remote change");
        run_git(seed.path(), &["push", "main", "main"]);

        let result = crate::commands::git::git_sync(local.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        assert!(result.fetched, "expected fetch to succeed: {}", result.message);
        assert!(result.pulled, "expected fast-forward pull: {}", result.message);
        assert_eq!(result.new_commits, 1);
        assert!(local.path().join("remote-change.txt").exists());
    }

    #[test]
    fn test_count_status_worktree_isolation() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // Commit a baseline file
        stage_file(&repo, _repo.path(), "base.txt", "base");
        commit_staged(&repo, "add base");

        // Create worktree OUTSIDE the main repo (avoids showing as untracked in main)
        let wt_dir = TempGitRepo::new();
        let wt_path = wt_dir.path().to_path_buf();
        repo.branch("wt-branch", &repo.head().unwrap().peel_to_commit().unwrap(), false).unwrap();
        repo.worktree("worktree1", &wt_path, None).unwrap();

        // Make a change ONLY in the worktree
        fs::write(wt_path.join("wt-only.txt"), "worktree change").unwrap();

        // Open repo at worktree path — should only see worktree changes
        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        let (total, added, _modified, _deleted) = count_status(&wt_repo);
        assert_eq!(total, 1, "worktree should only see its own changes");
        assert_eq!(added, 1);

        // Main repo should NOT see the worktree-only file
        let (main_total, _, _, _) = count_status(&repo);
        assert_eq!(main_total, 0, "main repo should not see worktree changes");
    }

    // ─── get_git_status line-stat tests ────────────────────────────────────────

    /// Look up a single changed file in the result, asserting it exists.
    fn find_status<'a>(
        statuses: &'a [crate::commands::git::GitStatus],
        path: &str,
    ) -> &'a crate::commands::git::GitStatus {
        statuses
            .iter()
            .find(|s| s.path == path)
            .unwrap_or_else(|| panic!("expected status entry for {path}, got {statuses:?}"))
    }

    #[test]
    fn get_git_status_counts_additions_for_untracked_file() {
        let _repo = TempGitRepo::new();
        let _ = init_git_repo(_repo.path());

        // Brand-new, unstaged file with three lines.
        fs::write(_repo.path().join("new.txt"), "x\ny\nz\n").unwrap();

        let statuses =
            crate::commands::git::get_git_status(_repo.path().to_string_lossy().into_owned())
                .unwrap();

        let entry = find_status(&statuses, "new.txt");
        assert_eq!(entry.status, "added");
        assert_eq!(entry.additions, 3, "three new lines");
        assert_eq!(entry.deletions, 0);
    }

    #[test]
    fn get_git_status_counts_additions_and_deletions_for_modified_file() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // Commit a baseline, then edit it: change one line, append one line.
        stage_file(&repo, _repo.path(), "edit.txt", "a\nb\nc\n");
        commit_staged(&repo, "add edit.txt");
        fs::write(_repo.path().join("edit.txt"), "a\nB\nc\nd\n").unwrap();

        let statuses =
            crate::commands::git::get_git_status(_repo.path().to_string_lossy().into_owned())
                .unwrap();

        let entry = find_status(&statuses, "edit.txt");
        assert_eq!(entry.status, "modified");
        // b -> B is one deletion + one addition; trailing d is one addition.
        assert_eq!(entry.additions, 2);
        assert_eq!(entry.deletions, 1);
    }

    #[test]
    fn get_git_status_counts_deletions_for_deleted_file() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        stage_file(&repo, _repo.path(), "gone.txt", "p\nq\n");
        commit_staged(&repo, "add gone.txt");
        fs::remove_file(_repo.path().join("gone.txt")).unwrap();

        let statuses =
            crate::commands::git::get_git_status(_repo.path().to_string_lossy().into_owned())
                .unwrap();

        let entry = find_status(&statuses, "gone.txt");
        assert_eq!(entry.status, "deleted");
        assert_eq!(entry.additions, 0);
        assert_eq!(entry.deletions, 2, "both committed lines removed");
    }

    #[test]
    fn get_git_status_reports_zero_stats_for_clean_repo() {
        let _repo = TempGitRepo::new();
        let _ = init_git_repo(_repo.path());

        let statuses =
            crate::commands::git::get_git_status(_repo.path().to_string_lossy().into_owned())
                .unwrap();

        assert!(statuses.is_empty(), "clean repo has no changed files: {statuses:?}");
    }

    // ─── get_git_branch_status ──────────────────────────────────────────────

    #[test]
    fn branch_status_returns_none_for_non_git_dir() {
        let dir = TempGitRepo::new();
        fs::create_dir_all(dir.path()).unwrap();

        let result =
            crate::commands::git::get_git_branch_status(dir.path().to_string_lossy().into_owned())
                .unwrap();
        assert!(result.is_none(), "non-git dir should return None");
    }

    #[test]
    fn branch_status_reports_branch_name_with_no_upstream() {
        let repo_dir = TempGitRepo::new();
        init_git_repo(repo_dir.path());
        run_git(repo_dir.path(), &["branch", "-M", "main"]);

        let status =
            crate::commands::git::get_git_branch_status(repo_dir.path().to_string_lossy().into_owned())
                .unwrap()
                .expect("repo with a commit should report a branch");

        assert_eq!(status.name, "main");
        assert_eq!(status.ahead, 0, "no upstream → ahead is 0");
        assert_eq!(status.behind, 0, "no upstream → behind is 0");
        assert!(!status.short_sha.is_empty(), "repo with a commit should report a short SHA");
        assert_eq!(status.short_sha.len(), 7, "git2 short_id() defaults to 7 chars");
    }

    #[test]
    fn branch_status_reports_empty_short_sha_for_unborn_head() {
        // Unborn HEAD: repo initialised but no commits.
        let dir = TempGitRepo::new();
        let _repo = git2::Repository::init(dir.path()).unwrap();

        // get_git_branch_status returns Ok(None) for an unborn HEAD (the existing
        // path in get_git_branch_status short-circuits on `repo.head()` Err).
        let result =
            crate::commands::git::get_git_branch_status(dir.path().to_string_lossy().into_owned())
                .unwrap();
        assert!(result.is_none(), "unborn HEAD should return None");
    }

    #[test]
    fn branch_status_counts_behind_when_remote_is_ahead() {
        let remote = TempGitRepo::new();
        fs::create_dir_all(remote.path()).unwrap();
        git2::Repository::init_bare(remote.path()).unwrap();

        let seed = TempGitRepo::new();
        let seed_repo = init_git_repo(seed.path());
        run_git(seed.path(), &["branch", "-M", "main"]);
        run_git(seed.path(), &["remote", "add", "origin", remote.path().to_str().unwrap()]);
        run_git(seed.path(), &["push", "-u", "origin", "main"]);

        let local = TempGitRepo::new();
        run_git_without_dir(&[
            "clone",
            "-b",
            "main",
            remote.path().to_str().unwrap(),
            local.path().to_str().unwrap(),
        ]);

        // Advance the remote by one commit, then fetch into the local clone
        // (fetch updates the tracking ref without moving local HEAD).
        stage_file(&seed_repo, seed.path(), "remote-change.txt", "remote change");
        commit_staged(&seed_repo, "add remote change");
        run_git(seed.path(), &["push", "origin", "main"]);
        run_git(local.path(), &["fetch", "origin"]);

        let status =
            crate::commands::git::get_git_branch_status(local.path().to_string_lossy().into_owned())
                .unwrap()
                .expect("clone should report a branch");

        assert_eq!(status.name, "main");
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 1, "remote advanced by one commit");
    }

    #[test]
    fn branch_status_counts_ahead_when_local_is_ahead() {
        let remote = TempGitRepo::new();
        fs::create_dir_all(remote.path()).unwrap();
        git2::Repository::init_bare(remote.path()).unwrap();

        let local = TempGitRepo::new();
        let local_repo = init_git_repo(local.path());
        run_git(local.path(), &["branch", "-M", "main"]);
        run_git(local.path(), &["remote", "add", "origin", remote.path().to_str().unwrap()]);
        run_git(local.path(), &["push", "-u", "origin", "main"]);

        // Commit locally without pushing.
        stage_file(&local_repo, local.path(), "local-change.txt", "local change");
        commit_staged(&local_repo, "add local change");

        let status =
            crate::commands::git::get_git_branch_status(local.path().to_string_lossy().into_owned())
                .unwrap()
                .expect("repo should report a branch");

        assert_eq!(status.name, "main");
        assert_eq!(status.ahead, 1, "one unpushed local commit");
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn branch_status_reports_short_sha_for_detached_head() {
        // Applies to agents in `detached` worktree mode and to `branched`
        // agents detached by `free_base_branch` recovery (ADR 0006). We must
        // still report a short SHA so the File Explorer header can render
        // "detached @ a064f55".
        let dir = TempGitRepo::new();
        let repo = init_git_repo(dir.path());
        let head_oid = repo.head().unwrap().target().unwrap();

        // Detach HEAD at the current commit.
        repo.set_head_detached(head_oid).unwrap();

        let status =
            crate::commands::git::get_git_branch_status(dir.path().to_string_lossy().into_owned())
                .unwrap()
                .expect("detached HEAD on a commit should still report a status");

        assert_eq!(status.name, "HEAD", "shorthand() of a detached HEAD is \"HEAD\"");
        assert_eq!(
            status.short_sha, head_oid.to_string().get(..7).unwrap(),
            "short_sha should be the 7-char prefix of the HEAD OID"
        );
        assert_eq!(status.short_sha.len(), 7);
    }

    // ─── get_mesh_git_static gh-auth cache (issue #432) ─────────────────────

    /// Five back-to-back `get_mesh_git_static` calls within the TTL window
    /// must result in a single underlying `check_gh_auth` round-trip, not
    /// five. This is the acceptance criterion for the #432 cache: a user
    /// iterating through N meshes in the sidebar pays 1 gh round-trip (the
    /// first mesh) + 1 per TTL expiry, instead of 1 per mesh.
    ///
    /// The cache must live in `get_mesh_git_static`, NOT in
    /// `commands::pr::check_gh_auth` itself: that command is also called by
    /// the mobile `/git/auth` HTTP route (`http/routes/git.rs:112`) and by
    /// `MeshPropertiesTab.tsx` when the user clicks "re-check", and both
    /// want a fresh value.
    #[test]
    fn get_mesh_git_static_caches_gh_auth_across_calls() {
        // Force a known starting state: miss counter = 0, cache backdated
        // so the next read is a miss.
        crate::commands::git::__reset_gh_auth_cache_for_tests();

        // Non-existent paths are fine — we just want to exercise the cache
        // path. `is_git_repo` will be `false` for all of them, but the
        // gh-auth branch runs unconditionally (a non-git dir can still have
        // `gh` configured), which is exactly the path we're caching.
        let r1 = crate::commands::git::get_mesh_git_static("/tmp/fake-mesh-1".to_string())
            .expect("first call should succeed");
        let r2 = crate::commands::git::get_mesh_git_static("/tmp/fake-mesh-2".to_string())
            .expect("second call should succeed");
        let r3 = crate::commands::git::get_mesh_git_static("/tmp/fake-mesh-3".to_string())
            .expect("third call should succeed");
        let r4 = crate::commands::git::get_mesh_git_static("/tmp/fake-mesh-4".to_string())
            .expect("fourth call should succeed");
        let r5 = crate::commands::git::get_mesh_git_static("/tmp/fake-mesh-5".to_string())
            .expect("fifth call should succeed");

        // All 5 must agree on the process-wide gh-auth state. The value
        // itself depends on the env (GITHUB_TOKEN / GH_TOKEN / `gh auth
        // status`); we don't assert what it is, only that it's stable.
        assert_eq!(r1.is_gh_authenticated, r2.is_gh_authenticated);
        assert_eq!(r2.is_gh_authenticated, r3.is_gh_authenticated);
        assert_eq!(r3.is_gh_authenticated, r4.is_gh_authenticated);
        assert_eq!(r4.is_gh_authenticated, r5.is_gh_authenticated);

        // The headline assertion: 5 snapshot calls = 1 gh round-trip.
        assert_eq!(
            crate::commands::git::__gh_auth_cache_misses(),
            1,
            "5 get_mesh_git_static calls within the TTL window should result in exactly 1 gh round-trip"
        );
    }

    #[test]
    fn test_count_status_main_repo_vs_worktree() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        // Commit baseline
        stage_file(&repo, _repo.path(), "base.txt", "base");
        commit_staged(&repo, "add base");

        // Create worktree OUTSIDE the main repo
        let wt_dir = TempGitRepo::new();
        let wt_path = wt_dir.path().to_path_buf();
        repo.branch("wt-branch2", &repo.head().unwrap().peel_to_commit().unwrap(), false).unwrap();
        repo.worktree("worktree2", &wt_path, None).unwrap();

        // Make changes in the main working tree
        fs::write(_repo.path().join("main-change.txt"), "main only").unwrap();

        // Make changes in the worktree
        fs::write(wt_path.join("wt-change.txt"), "wt only").unwrap();

        // Main repo sees only main changes
        let (main_total, main_added, _, _) = count_status(&repo);
        assert_eq!(main_total, 1, "main sees only its own changes");
        assert_eq!(main_added, 1);

        // Worktree sees only worktree changes
        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        let (wt_total, wt_added, _, _) = count_status(&wt_repo);
        assert_eq!(wt_total, 1, "worktree sees only its own changes");
        assert_eq!(wt_added, 1);
    }
}
