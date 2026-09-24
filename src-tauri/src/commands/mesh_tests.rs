//! Tests for the mesh command layer: the clone destination guard, git's
//! error-line extraction, and the end-to-end clone orchestration driven against
//! a local fixture repository.
//!
//! The clone tests stand up the shared process-global DB via `db::init` (which is
//! one-shot), so they serialise on [`tests::MESH_CLONE_TEST_LOCK`] and follow the
//! `ensure_*_db` pattern from `commands::pr`. Run this module with
//! `--test-threads=1` for a trustworthy verdict, like every other DB-touching
//! test in this binary.

#[cfg(test)]
mod tests {
    use crate::commands::mesh::{
        clone_target_into_mesh, first_error_line, resolve_clone_destination,
    };
    use crate::services::github::CloneTarget;

    /// Serialises this module's DB-touching clone tests.
    static MESH_CLONE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        MESH_CLONE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Init the shared process-global DB the first time a test needs it; a no-op
    /// thereafter (`db::init` is one-shot).
    fn ensure_db() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let path = std::env::temp_dir().join(format!(
                "buildmesh_mesh_clone_test_{}.db",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            let _ = crate::db::init(&path);
        });
    }

    /// Run `git` through the shared no-prompt wrapper; true on success. Using the
    /// wrapper (rather than a bare spawn) also keeps the CI inline-spawn guard
    /// quiet and gives the fixture the same env production git runs under.
    fn run_git(dir: &std::path::Path, args: &[&str]) -> bool {
        crate::process_util::git_command()
            .args(args)
            .current_dir(dir)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /// A local source repository with one commit on a `trunk` branch —
    /// deliberately *not* `main`, so the base-ref assertion below proves the ref
    /// is resolved from the clone rather than hardcoded to `origin/main`.
    fn fixture_source_repo(dir: &std::path::Path) {
        assert!(run_git(dir, &["init", "-q"]), "git init");
        assert!(
            run_git(dir, &["symbolic-ref", "HEAD", "refs/heads/trunk"]),
            "point HEAD at trunk"
        );
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        assert!(run_git(dir, &["add", "."]), "git add");
        assert!(
            run_git(
                dir,
                &[
                    "-c",
                    "user.email=test@buildmesh.local",
                    "-c",
                    "user.name=Buildmesh Test",
                    "commit",
                    "-q",
                    "-m",
                    "init",
                ],
            ),
            "git commit"
        );
    }

    #[test]
    fn resolve_clone_destination_rejects_a_missing_parent() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("nope");

        let err = resolve_clone_destination(&missing.to_string_lossy(), "buildmesh")
            .expect_err("a missing parent folder must be rejected");
        assert!(err.contains("does not exist"), "unexpected error: {err}");
    }

    #[test]
    fn resolve_clone_destination_rejects_an_occupied_destination() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("buildmesh");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("README.md"), "existing").unwrap();

        let err = resolve_clone_destination(&temp.path().to_string_lossy(), "buildmesh")
            .expect_err("a non-empty destination must be rejected");
        assert!(err.contains("already exists"), "unexpected error: {err}");
    }

    /// A regular *file* at the destination is not a directory, so `read_dir`
    /// fails — the guard must not read that failure as "empty" and wave the clone
    /// through into a guaranteed `git clone` error.
    #[test]
    fn resolve_clone_destination_rejects_a_file_at_the_destination() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("buildmesh"), "not a directory").unwrap();

        let err = resolve_clone_destination(&temp.path().to_string_lossy(), "buildmesh")
            .expect_err("an existing file must be rejected, not treated as an empty dir");
        assert!(
            err.contains("A file already exists"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_clone_destination_allows_a_fresh_or_empty_destination() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().to_string_lossy().to_string();

        let (fresh, existed) = resolve_clone_destination(&parent, "buildmesh").unwrap();
        assert_eq!(fresh, temp.path().join("buildmesh"));
        assert!(!existed, "a not-yet-created destination must report existed=false");

        // git clones into a pre-existing *empty* directory, so that must pass too.
        std::fs::create_dir(&fresh).unwrap();
        let (same, existed) = resolve_clone_destination(&parent, "buildmesh").unwrap();
        assert_eq!(same, fresh);
        assert!(existed, "an empty pre-existing destination must report existed=true");
    }

    #[test]
    fn first_error_line_prefers_gits_fatal_line_over_progress_chatter() {
        // The real shape of a failed `git clone`: progress chatter lands on
        // stderr before the fatal line, so the first line is the useless one.
        let stderr = "Cloning into 'buildmesh'...\n\
                      remote: Repository not found.\n\
                      fatal: repository 'https://github.com/owner/buildmesh.git/' not found\n";
        assert_eq!(
            first_error_line(stderr),
            Some("fatal: repository 'https://github.com/owner/buildmesh.git/' not found")
        );
    }

    #[test]
    fn first_error_line_prefers_an_error_line_else_the_last_line() {
        assert_eq!(
            first_error_line("Cloning into 'x'...\nerror: remote not found\nhelp: see docs\n"),
            Some("error: remote not found")
        );
        // No `fatal:`/`error:` marker — the last non-empty line beats the first.
        assert_eq!(
            first_error_line("Cloning into 'x'...\nwarning: retrying\n"),
            Some("warning: retrying")
        );
        assert_eq!(first_error_line(""), None);
        assert_eq!(first_error_line("  \n\n"), None);
    }

    /// Drives the whole orchestration for real — destination guard, `git clone`
    /// through the shared wrapper, default-branch resolution, the mesh row, and
    /// the colour/hook finisher — against a local fixture repository. Uses the
    /// [`clone_target_into_mesh`] seam because `parse_clone_input` (rightly) only
    /// admits github.com URLs, which no offline test can reach.
    #[test]
    fn clone_target_into_mesh_clones_locally_and_resolves_the_default_branch() {
        let _serial = serial();
        ensure_db();

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        fixture_source_repo(&source);

        let parent = temp.path().join("parent");
        std::fs::create_dir(&parent).unwrap();

        let target = CloneTarget {
            url: source.to_string_lossy().to_string(),
            repo: "fixture-repo".to_string(),
        };

        let mesh = clone_target_into_mesh(&target, &parent.to_string_lossy(), None)
            .expect("cloning a local fixture repository must succeed");

        assert_eq!(mesh.name, "fixture-repo");
        assert_eq!(
            std::path::PathBuf::from(&mesh.path),
            parent.join("fixture-repo")
        );
        // `trunk` is the source's default branch, so a hardcoded `origin/main`
        // would fail here.
        assert_eq!(mesh.base_ref, "origin/trunk");
        assert_eq!(
            crate::db::get_mesh_by_id(mesh.id).unwrap().base_ref,
            "origin/trunk"
        );
        assert!(
            parent.join("fixture-repo").join("README.md").exists(),
            "the cloned working tree should be on disk"
        );
    }

    /// The failure half of the same path: git's real diagnostic reaches the
    /// caller and the partial tree is cleared, so the obvious retry is not
    /// blocked by the collision guard.
    #[test]
    fn clone_target_into_mesh_reports_gits_error_and_leaves_no_partial_tree() {
        let _serial = serial();
        ensure_db();

        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let nowhere = temp.path().join("nowhere");

        let target = CloneTarget {
            url: nowhere.to_string_lossy().to_string(),
            repo: "ghost-repo".to_string(),
        };

        let err = clone_target_into_mesh(&target, &parent.to_string_lossy(), None)
            .expect_err("cloning a missing repository must fail");

        assert!(err.contains("git clone failed"), "unexpected error: {err}");
        assert!(
            !parent.join("ghost-repo").exists(),
            "a failed clone must not leave a partial tree behind"
        );
    }
}
