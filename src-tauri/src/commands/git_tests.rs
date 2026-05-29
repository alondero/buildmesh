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
