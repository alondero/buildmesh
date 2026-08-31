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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

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

    /// Read the git2 status flags for a single repo-relative path. Returns
    /// `StatusFlags::empty()` when the path has no status entry — call sites
    /// typically follow up with an explicit `is_*` predicate. Used by
    /// `stage_file` / `revert_file` tests that need worktree-vs-index
    /// precision (`count_status` collapses those flags into shared
    /// buckets). Issue #1374.
    fn flags_for_path(repo: &git2::Repository, file_path: &str) -> git2::Status {
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let statuses = repo.statuses(Some(&mut opts)).unwrap();
        for entry in statuses.iter() {
            if entry.path().unwrap_or("") == file_path {
                return entry.status();
            }
        }
        git2::Status::empty()
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

    /// 2026-07-17 incident (pixelcache): a mesh whose `origin` was wired
    /// URL-only — `remote.origin.url` present, `remote.origin.fetch`
    /// refspec MISSING — reported "Already up to date" from the sidebar
    /// Sync button while a terminal `git pull` in the same directory
    /// pulled a five-days' backlog. Two compounding failures:
    /// `git fetch origin` updated only FETCH_HEAD (no refspec → no
    /// tracking-ref update), and git2's refspec-based upstream mapping
    /// failed so the behind-count error was swallowed into `UpToDate`.
    /// The command must instead fetch (explicit refspec), count via the
    /// branch-config fallback, and fast-forward.
    #[tokio::test]
    async fn git_sync_pulls_when_remote_has_no_fetch_refspec() {
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
        // Recreate the incident config: URL-only remote. The clone wrote
        // the standard refspec; drop it, and drop the now-unmappable
        // tracking ref state back to the clone-time SHA (it's already
        // there — the remote hasn't moved yet).
        run_git(local.path(), &["config", "--unset-all", "remote.origin.fetch"]);

        // Remote gains a commit AFTER the clone.
        stage_file(&seed_repo, seed.path(), "remote-change.txt", "remote change");
        commit_staged(&seed_repo, "add remote change");
        run_git(seed.path(), &["push", "origin", "main"]);

        let result = crate::commands::git::git_sync(local.path().to_string_lossy().into_owned())
            .await
            .unwrap();

        assert!(result.fetched, "expected fetch to succeed: {}", result.message);
        assert!(
            result.pulled,
            "expected fast-forward pull despite the missing fetch refspec \
             (got: {})",
            result.message
        );
        assert_eq!(result.new_commits, 1, "message: {}", result.message);
        assert!(local.path().join("remote-change.txt").exists());
        // Confidence contract (ADR 0022): a successful manual Sync must
        // stamp the freshness registry so a subsequent spawn within the
        // TTL skips its own fetch. The spawn path's `spawn_can_skip_fetch`
        // gate reads `time_since_success`, so that's the call we check.
        assert!(
            crate::services::fetch_freshness::time_since_success(local.path().to_str().unwrap())
                < std::time::Duration::from_secs(60),
            "manual Sync that pulled new commits must stamp the freshness \
             registry — without it the user's confidence contract fails on \
             the very next spawn"
        );
    }

    /// Regression test for issue #680 — the manual `git_sync` Tauri
    /// command must run inside `services::sync_lock::with_mesh_sync_lock`
    /// so a manual Sync click on a Mesh can't race against concurrent
    /// spawn-time `fetch_origin` calls (or against a second manual Sync)
    /// for the same Mesh. Without the wrap, two `git fetch` shell-outs
    /// collide on `.git/FETCH_HEAD` / `refs/heads/<branch>.lock`.
    ///
    /// Strategy: a holder thread sets a flag once it's inside the
    /// per-mesh lock and sleeps there; the test waits on the flag
    /// (deterministic — no `thread::sleep` race), then times `git_sync`.
    /// With the wrap, `git_sync` blocks ~450 ms waiting for the holder;
    /// without, it runs concurrently with the holder and finishes in
    /// tens of ms.
    ///
    /// Why wall-clock (not `fetch_add`): the per-mesh lock is correctly
    /// implemented (issue #652 tests), so it *prevents* simultaneous
    /// critical-section entries — `max_concurrent == 1` even on a
    /// working lock. The only signal that `git_sync` shares the same
    /// lock key is that it waits for the holder to release.
    ///
    /// Test setup deliberately leaves the clone at the same commit as
    /// the remote so `do_sync` returns `UpToDate` — that keeps the
    /// outcome out of the `Synced` / `FetchedButDiverged` branches
    /// (`git_sync` looks up the Mesh in the DB to fire the warm-pool
    /// freshness pass, and the test binary doesn't initialise the DB).
    /// `UpToDate` still goes through the full `git fetch` shell-out,
    /// which is the operation we need to serialize.
    #[tokio::test(flavor = "multi_thread")]
    async fn git_sync_serializes_via_per_mesh_sync_lock_gh680() {
        use std::sync::atomic::AtomicUsize;

        let remote = TempGitRepo::new();
        fs::create_dir_all(remote.path()).unwrap();
        git2::Repository::init_bare(remote.path()).unwrap();

        let seed = TempGitRepo::new();
        init_git_repo(seed.path());
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

        let path_key = local.path().to_string_lossy().into_owned();

        // Holder enters the per-mesh lock and announces entry via
        // `entered_flag` before sleeping, so the main thread can
        // deterministically start timing only once the holder holds
        // the lock (no `thread::sleep` race — CI jitter can't make
        // `git_sync` sneak in first).
        let entered_flag = Arc::new(AtomicUsize::new(0));
        let holder_path = path_key.clone();
        let entered_holder = Arc::clone(&entered_flag);
        let holder = thread::spawn(move || {
            crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
                entered_holder.store(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(500));
            });
        });

        // Spin-wait (bounded) for the holder to actually be inside the
        // critical section. Cap at 2s so a hung holder surfaces as a
        // test panic, not a forever-wait.
        let deadline = Instant::now() + Duration::from_secs(2);
        while entered_flag.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "holder thread never entered the per-mesh lock"
            );
            thread::sleep(Duration::from_millis(1));
        }

        let start = Instant::now();
        let _ = crate::commands::git::git_sync(path_key.clone()).await;
        let elapsed = start.elapsed();

        holder.join().unwrap();

        // With wrap: elapsed >= ~450 ms (`git_sync` waited for holder).
        // Without wrap: elapsed = tens of ms (`git_sync` ran
        // concurrently with the holder's sleep). Bound is 400 ms —
        // leaves 100 ms of slack for setup overhead and CI jitter.
        assert!(
            elapsed >= Duration::from_millis(400),
            "git_sync did not block on the per-mesh lock (elapsed = {:?}); \
             issue #680 wrap is missing — concurrent manual Sync and \
             spawn-time fetch_origin would race on .git/FETCH_HEAD",
            elapsed,
        );
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
            crate::commands::git::get_git_status_blocking(_repo.path().to_string_lossy().into_owned())
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
            crate::commands::git::get_git_status_blocking(_repo.path().to_string_lossy().into_owned())
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
            crate::commands::git::get_git_status_blocking(_repo.path().to_string_lossy().into_owned())
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
            crate::commands::git::get_git_status_blocking(_repo.path().to_string_lossy().into_owned())
                .unwrap();

        assert!(statuses.is_empty(), "clean repo has no changed files: {statuses:?}");
    }

    // ─── get_git_branch_status ──────────────────────────────────────────────

    #[test]
    fn branch_status_returns_none_for_non_git_dir() {
        let dir = TempGitRepo::new();
        fs::create_dir_all(dir.path()).unwrap();

        let result =
            crate::commands::git::get_git_branch_status_blocking(dir.path().to_string_lossy().into_owned())
                .unwrap();
        assert!(result.is_none(), "non-git dir should return None");
    }

    #[test]
    fn branch_status_reports_branch_name_with_no_upstream() {
        let repo_dir = TempGitRepo::new();
        init_git_repo(repo_dir.path());
        run_git(repo_dir.path(), &["branch", "-M", "main"]);

        let status =
            crate::commands::git::get_git_branch_status_blocking(repo_dir.path().to_string_lossy().into_owned())
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
            crate::commands::git::get_git_branch_status_blocking(dir.path().to_string_lossy().into_owned())
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
            crate::commands::git::get_git_branch_status_blocking(local.path().to_string_lossy().into_owned())
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
            crate::commands::git::get_git_branch_status_blocking(local.path().to_string_lossy().into_owned())
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
            crate::commands::git::get_git_branch_status_blocking(dir.path().to_string_lossy().into_owned())
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
    /// `commands::github::check_gh_auth` itself: that command is also called by
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
        // Exercise the sync core directly (the async `#[command]` wrapper
        // just offloads this onto the blocking pool via `run_blocking`).
        let r1 = crate::commands::git::get_mesh_git_static_blocking("/tmp/fake-mesh-1".to_string())
            .expect("first call should succeed");
        let r2 = crate::commands::git::get_mesh_git_static_blocking("/tmp/fake-mesh-2".to_string())
            .expect("second call should succeed");
        let r3 = crate::commands::git::get_mesh_git_static_blocking("/tmp/fake-mesh-3".to_string())
            .expect("third call should succeed");
        let r4 = crate::commands::git::get_mesh_git_static_blocking("/tmp/fake-mesh-4".to_string())
            .expect("fourth call should succeed");
        let r5 = crate::commands::git::get_mesh_git_static_blocking("/tmp/fake-mesh-5".to_string())
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

    // ─── get_mesh_git_static single-open refactor (issue #431) ─────────────

    /// Create a repo whose `refs/remotes/origin/HEAD` symbolic ref points at
    /// `refs/remotes/origin/<branch>`. Mirrors the setup pattern used in
    /// `agent/spawn.rs::tests::resolve_base_ref_*`.
    fn make_repo_with_origin_head(path: &Path, branch: &str) -> git2::Repository {
        let repo = init_git_repo(path);
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference(
            &format!("refs/remotes/origin/{branch}"),
            oid,
            true,
            "test setup",
        )
        .unwrap();
        let branch_ref = format!("refs/remotes/origin/{branch}");
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            &branch_ref,
            true,
            "test setup",
        )
        .unwrap();
        repo
    }

    /// `default_branch_from_repo` returns the branch the origin/HEAD
    /// symbolic ref points at, with the `refs/remotes/origin/` prefix
    /// stripped.
    #[test]
    fn default_branch_from_repo_returns_origin_head_target() {
        let _repo = TempGitRepo::new();
        let repo = make_repo_with_origin_head(_repo.path(), "master");

        let branch = crate::commands::git::default_branch_from_repo(&repo);
        assert_eq!(branch, "master");
    }

    /// `default_branch_from_repo` falls back to `"main"` when the repo has
    /// no origin/HEAD symbolic ref (e.g. freshly initialised, no fetch).
    #[test]
    fn default_branch_from_repo_falls_back_to_main_without_origin_head() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());

        let branch = crate::commands::git::default_branch_from_repo(&repo);
        assert_eq!(branch, "main");
    }

    /// Trio must reuse the repo handle opened for `is_git_repo` to resolve
    /// `default_branch` — not open the repo a second time (#431).
    #[test]
    fn get_mesh_git_static_reports_origin_head_branch_for_valid_repo() {
        let _repo = TempGitRepo::new();
        let _ = make_repo_with_origin_head(_repo.path(), "develop");

        let snapshot =
            crate::commands::git::get_mesh_git_static_blocking(_repo.path().to_string_lossy().into_owned())
                .expect("snapshot should succeed for a valid repo");

        assert!(
            snapshot.is_git_repo,
            "valid repo must report is_git_repo = true"
        );
        assert_eq!(
            snapshot.default_branch, "develop",
            "refactored trio must read default_branch from the same handle it used for is_git_repo"
        );
    }

    /// Non-git directory must not panic and must report the documented
    /// non-repo contract (`is_git_repo = false`, `default_branch = "main"`).
    #[test]
    fn get_mesh_git_static_reports_main_fallback_for_non_repo() {
        let dir = TempGitRepo::new();
        fs::create_dir_all(dir.path()).unwrap();

        let snapshot =
            crate::commands::git::get_mesh_git_static_blocking(dir.path().to_string_lossy().into_owned())
                .expect("non-repo path must not error");

        assert!(!snapshot.is_git_repo);
        assert_eq!(
            snapshot.default_branch, "main",
            "non-repo path falls back to main, mirroring get_default_branch's open-failed branch"
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

    // ─── Per-file stage / revert (issue #1374) ──────────────────────────────

    use crate::commands::git::{revert_file_blocking, stage_file_blocking};

    #[test]
    fn reject_traversal_blocks_dotdot_and_absolute_paths() {
        // Tested indirectly through the public blocking functions, which
        // call reject_traversal before any filesystem access.
        assert!(stage_file_blocking("/tmp/repo", "../outside.txt").is_err());
        assert!(stage_file_blocking("/tmp/repo", "/etc/passwd").is_err());
        assert!(revert_file_blocking("/tmp/repo", "src/../../../etc/passwd").is_err());
        // `..bar` is a valid POSIX filename but escapes the worktree when
        // joined onto the host root; the gate must catch it too.
        assert!(stage_file_blocking("/tmp/repo", "src/..bar").is_err());
        assert!(revert_file_blocking("/tmp/repo", "..bar").is_err());
    }

    #[test]
    fn stage_file_stages_untracked_and_modified_files() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());
        let path = _repo.path().to_str().unwrap();

        // Stage an untracked file → index_new.
        fs::write(_repo.path().join("new.txt"), "hello").unwrap();
        stage_file_blocking(path, "new.txt").unwrap();
        let (total, added, _, _) = count_status(&repo);
        assert_eq!((total, added), (1, 1), "staged untracked = index_new");

        // Commit, edit, re-stage → no worktree modification remains. The
        // file is still INDEX_MODIFIED (staged-but-not-committed), so we
        // assert directly on the worktree-side flag, not on count_status'
        // combined bucket (which lumps INDEX_MODIFIED + WT_MODIFIED).
        commit_staged(&repo, "add new.txt");
        fs::write(_repo.path().join("new.txt"), "edited").unwrap();
        let wt_pre = flags_for_path(&repo, "new.txt");
        assert!(wt_pre.is_wt_modified(), "precondition: worktree edit is unstaged");
        stage_file_blocking(path, "new.txt").unwrap();
        let wt_post = flags_for_path(&repo, "new.txt");
        assert!(!wt_post.is_wt_modified(), "re-stage clears the worktree delta");
        assert!(wt_post.is_index_modified(), "re-stage still differs from HEAD");
    }

    #[test]
    fn stage_file_stages_deletion_of_tracked_file() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());
        let path = _repo.path().to_str().unwrap();

        stage_file(&repo, _repo.path(), "gone.txt", "to be deleted");
        commit_staged(&repo, "add gone.txt");
        fs::remove_file(_repo.path().join("gone.txt")).unwrap();

        // Precondition: worktree deletion is unstaged (WT_DELETED).
        let wt_pre = flags_for_path(&repo, "gone.txt");
        assert!(wt_pre.is_wt_deleted(), "precondition: file missing from worktree");

        stage_file_blocking(path, "gone.txt").unwrap();

        // The deletion is staged → INDEX_DELETED, not WT_DELETED (the index
        // also lacks the entry, so the worktree-vs-index comparison is
        // vacuously clean). count_status' "deleted" bucket would still be
        // 1 because it counts INDEX_DELETED; assert the precise flags.
        let wt_post = flags_for_path(&repo, "gone.txt");
        assert!(!wt_post.is_wt_deleted(), "staged deletion must not read as wt_deleted");
        assert!(wt_post.is_index_deleted(), "the entry must be index_deleted (staged deletion)");
    }

    #[test]
    fn stage_file_rejects_traversal() {
        let _repo = TempGitRepo::new();
        init_git_repo(_repo.path());
        let path = _repo.path().to_str().unwrap();
        assert!(stage_file_blocking(path, "../outside.txt").is_err());
        assert!(stage_file_blocking(path, "/etc/passwd").is_err());
    }

    #[test]
    fn revert_file_restores_tracked_file_to_head_state() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());
        let path = _repo.path().to_str().unwrap();

        stage_file(&repo, _repo.path(), "keep.txt", "original");
        commit_staged(&repo, "add keep.txt");

        // Uncommitted edit + staged edit → revert must clear both.
        fs::write(_repo.path().join("keep.txt"), "worktree edit").unwrap();
        stage_file(&repo, _repo.path(), "keep.txt", "staged edit");
        let (total, _, modified, _) = count_status(&repo);
        assert!(total >= 1 && modified >= 1, "precondition: dirty file");

        revert_file_blocking(path, "keep.txt").unwrap();

        let (total, _, modified, _) = count_status(&repo);
        assert_eq!((total, modified), (0, 0), "revert restores HEAD state exactly");
        assert_eq!(
            fs::read_to_string(_repo.path().join("keep.txt")).unwrap(),
            "original",
            "worktree content must match the HEAD blob"
        );
    }

    #[test]
    fn revert_file_deletes_file_not_in_head() {
        let _repo = TempGitRepo::new();
        let repo = init_git_repo(_repo.path());
        let path = _repo.path().to_str().unwrap();

        // A brand-new untracked file.
        fs::write(_repo.path().join("added.txt"), "brand new").unwrap();
        // A previously `git add`ed (staged) new file.
        stage_file(&repo, _repo.path(), "staged-new.txt", "staged only");

        revert_file_blocking(path, "added.txt").unwrap();
        revert_file_blocking(path, "staged-new.txt").unwrap();

        assert!(! _repo.path().join("added.txt").exists(), "untracked file removed");
        assert!(! _repo.path().join("staged-new.txt").exists(), "staged-new file removed");
        let (total, _, _, _) = count_status(&repo);
        assert_eq!(total, 0, "index entries dropped too");
    }

    #[test]
    fn revert_file_rejects_traversal() {
        let _repo = TempGitRepo::new();
        init_git_repo(_repo.path());
        let path = _repo.path().to_str().unwrap();
        assert!(revert_file_blocking(path, "../outside.txt").is_err());
        assert!(revert_file_blocking(path, "/etc/passwd").is_err());
    }
}
