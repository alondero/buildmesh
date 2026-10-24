// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::env::test_helpers::{init_repo_with_commit, TestDir};

/// A repository with one commit and two configured remotes.
fn repo_with_remotes() -> (TestDir, String) {
    let dir = TestDir::new("git_remote");
    let repo = init_repo_with_commit(dir.path(), &[("f.txt", "content\n")]);
    repo.remote("origin", "https://example.com/acme/repo.git")
        .expect("add origin");
    repo.remote("upstream", "https://example.com/upstream/repo.git")
        .expect("add upstream");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

/// The identity read names the checked-out branch, the origin URL and every
/// configured remote.
#[test]
fn repo_identity_reports_the_branch_the_origin_url_and_the_remote_names() {
    let (_dir, path) = repo_with_remotes();
    let repo = Repository::open(&path).expect("reopen repo");
    let commit = repo.head().expect("head").peel_to_commit().expect("commit");
    repo.branch("feature/x", &commit, false).expect("branch");
    repo.set_head("refs/heads/feature/x").expect("set head");

    let identity = repo_identity(&path).expect("identity");

    assert_eq!(identity.branch, "feature/x");
    assert_eq!(
        identity.remote_url.as_deref(),
        Some("https://example.com/acme/repo.git")
    );
    assert!(identity.remotes.contains(&"origin".to_string()));
    assert!(identity.remotes.contains(&"upstream".to_string()));
}

/// A repository with no commits has no branch to report — the identity read
/// says so with an empty branch rather than failing.
#[test]
fn repo_identity_of_an_unborn_head_has_no_branch() {
    let dir = TestDir::new("git_remote_unborn");
    Repository::init(dir.path()).expect("init repo");
    let path = dir.path().to_string_lossy().into_owned();

    let identity = repo_identity(&path).expect("identity");

    assert_eq!(identity.branch, "");
    assert!(identity.remote_url.is_none());
    assert!(identity.remotes.is_empty());
}

/// A detached HEAD has no branch name, so the identity read reports the
/// short commit SHA instead — the same shape the branch-status read uses.
#[test]
fn a_detached_head_reports_the_short_commit_sha() {
    let (_dir, path) = repo_with_remotes();
    let repo = Repository::open(&path).expect("reopen repo");
    let commit = repo.head().expect("head").peel_to_commit().expect("commit");
    repo.set_head_detached(commit.id()).expect("detach");

    let identity = repo_identity(&path).expect("identity");

    assert!(
        identity.branch.len() >= 7
            && identity
                .branch
                .chars()
                .all(|character| character.is_ascii_hexdigit()),
        "a detached HEAD must report a short commit sha, got {:?}",
        identity.branch
    );
}

/// The named remote's URL is read from the same repository the identity read
/// opens.
#[test]
fn remote_url_reads_the_named_remote() {
    let (_dir, path) = repo_with_remotes();

    assert_eq!(
        remote_url(&path, "upstream")
            .expect("remote url")
            .as_deref(),
        Some("https://example.com/upstream/repo.git")
    );
}

/// A remote that is not configured has no URL — `None`, not an error. Every
/// caller treats "no URL" as "nothing to show".
#[test]
fn remote_url_is_none_for_a_remote_that_is_not_configured() {
    let (_dir, path) = repo_with_remotes();

    assert!(remote_url(&path, "no-such-remote")
        .expect("remote url")
        .is_none());
}

/// A path that is not a repository reports a readable error rather than
/// panicking — the caller shows the message to the user.
#[test]
fn a_path_that_is_not_a_repository_reports_a_readable_error() {
    let dir = TestDir::new("git_remote_not_a_repo");
    let path = dir.path().to_string_lossy().into_owned();

    let error = repo_identity(&path).expect_err("must fail");

    assert!(
        error.starts_with("git error:"),
        "the error must read as a git error, got {error}"
    );
}

/// The safe-directory hint quotes the path the way the host's shell reads
/// it, so the user can paste the command as-is.
#[test]
fn safe_directory_command_quotes_the_path_for_the_host() {
    let command = safe_directory_command("C:\\repo\\path", cfg!(windows));

    assert!(
        command.starts_with("git config --global --add safe.directory "),
        "got {command}"
    );
    if cfg!(windows) {
        assert!(command.contains("C:/repo/path"), "got {command}");
    } else {
        assert!(command.contains("'C:\\repo\\path'"), "got {command}");
    }
}
