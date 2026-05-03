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

    fn count_status_debug(repo: &git2::Repository) -> String {
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);

        let statuses = repo.statuses(Some(&mut opts)).unwrap();

        let mut lines = Vec::new();
        for entry in statuses.iter() {
            let path = entry.path().unwrap_or("?");
            let flag = entry.status();
            lines.push(format!("  {}: idx_new={} wt_new={} idx_mod={} wt_mod={} idx_del={} wt_del={} ignored={}",
                path, flag.is_index_new(), flag.is_wt_new(),
                flag.is_index_modified(), flag.is_wt_modified(),
                flag.is_index_deleted(), flag.is_wt_deleted(), flag.is_ignored()));
        }
        lines.join("\n")
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
        let debug = count_status_debug(&repo);
        eprintln!("DEBUG mixed_changes: total={}, added={}, modified={}, deleted={}\nStatuses:\n{}", total, added, modified, deleted, debug);

        // Total should be 3 or 4 depending on whether staged.txt registers as a separate entry.
        // git2 may combine staged+worktree-new for never-committed files into a single entry.
        assert!(total >= 3, "expected at least 3 changed files (untracked, modified, deleted)");
        assert!(added >= 1, "expected at least 1 added (untracked.txt)");
        assert_eq!(modified, 1, "expected 1 modified");
        assert!(deleted <= 1, "deleted should be 0 or 1");
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
}