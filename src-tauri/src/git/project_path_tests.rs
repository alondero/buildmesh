use super::project::resolve;
use crate::env::test_helpers::{init_repo_with_commit, TestDir};

#[test]
fn missing_project_folder_reports_recovery_and_can_be_retried() {
    let td = TestDir::new("missing-project-folder");
    let selected = td.path().join("disconnected").join("project");
    let selected = selected.to_str().unwrap();

    let reason = resolve(selected).unwrap_err();
    assert!(reason.contains("Project folder unavailable"), "{reason}");
    assert!(reason.contains(selected), "{reason}");
    assert!(reason.contains("drive or network mapping"), "{reason}");
    assert!(reason.contains("retry"), "{reason}");
    assert!(!reason.contains("No working Git repository"), "{reason}");

    std::fs::create_dir_all(selected).unwrap();
    init_repo_with_commit(std::path::Path::new(selected), &[("README.md", "restored")]);
    assert!(resolve(selected).is_ok());
}

#[test]
fn accessible_non_git_folder_keeps_git_diagnostic() {
    let td = TestDir::new("accessible-non-git-project");
    let selected = td.path().to_str().unwrap();
    let reason = resolve(selected).unwrap_err();
    assert!(reason.contains("No working Git repository"), "{reason}");
    assert!(reason.contains(selected), "{reason}");
    assert!(!reason.contains("Project folder unavailable"), "{reason}");
}

#[test]
fn project_file_is_reported_as_not_a_directory() {
    let td = TestDir::new("project-is-file");
    let selected = td.path().join("file.txt");
    std::fs::write(&selected, "not a folder").unwrap();
    let selected = selected.to_str().unwrap();
    let reason = resolve(selected).unwrap_err();
    assert_eq!(
        reason,
        format!("Project folder {selected} is not a directory")
    );
}

#[test]
fn inaccessible_git_metadata_is_not_reported_as_missing_project_folder() {
    let td = TestDir::new("invalid-project-git-link");
    std::fs::write(td.path().join(".git"), "gitdir: missing-git-directory\n").unwrap();
    let reason = resolve(td.path().to_str().unwrap()).unwrap_err();
    assert!(reason.contains("No working Git repository"), "{reason}");
    assert!(!reason.contains("Project folder unavailable"), "{reason}");
}
