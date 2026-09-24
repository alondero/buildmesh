//! Unit tests for the mesh command helpers that need neither a network nor a
//! database: the clone destination guard and git's error-line extraction.
//!
//! Tempdir-only, so these are parallel-safe — unlike `db::mesh_tests`, which
//! contends on the shared process-global connection.

#[cfg(test)]
mod tests {
    use crate::commands::mesh::{first_error_line, resolve_clone_destination};

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
}
