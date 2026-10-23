// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;

/// A throwaway repository with one commit on `main` and a remote called
/// `origin` whose tracking ref `origin/feat-x` points at that commit.
fn repo_with_remote_tracking_ref() -> (tempfile::TempDir, git2::Oid) {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = Repository::init(dir.path()).expect("init repo");
    let signature = git2::Signature::now("test", "test@example.com").expect("signature");
    let tree_id = repo
        .treebuilder(None)
        .expect("treebuilder")
        .write()
        .expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let head_oid = repo
        .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .expect("commit");

    // `origin/feat-x` is a remote-tracking ref: the spawn path reads it
    // after `git fetch origin -- feat-x` populates it. A loose ref file is
    // enough for libgit2 to resolve it, and it keeps the repository valid
    // for the git CLI too (a hand-written `packed-refs` line in the wrong
    // order makes each later `git` command reject the repository).
    let refs_dir = repo.path().join("refs/remotes/origin");
    std::fs::create_dir_all(&refs_dir).expect("refs dir");
    std::fs::write(refs_dir.join("feat-x"), head_oid.to_string()).expect("write ref");
    (dir, head_oid)
}

#[test]
fn origin_ref_resolves_exactly() {
    let (_dir, head) = repo_with_remote_tracking_ref();
    let repo = Repository::open(_dir.path()).expect("reopen repo");
    let oid = resolve_sha(&repo, &ShaSource::OriginRef("origin/feat-x".to_string()))
        .expect("origin ref must resolve");
    assert_eq!(oid, head);
}

#[test]
fn missing_origin_ref_is_an_error_not_a_head_fallback() {
    let (dir, _head) = repo_with_remote_tracking_ref();
    let repo = Repository::open(dir.path()).expect("reopen repo");
    let result = resolve_sha(
        &repo,
        &ShaSource::OriginRef("origin/does-not-exist".to_string()),
    );
    assert!(
        result.is_err(),
        "a missing remote-tracking ref must NOT silently resolve to HEAD"
    );
}

#[test]
fn local_head_resolves_the_head_commit() {
    let (dir, head) = repo_with_remote_tracking_ref();
    let repo = Repository::open(dir.path()).expect("reopen repo");
    let oid = resolve_sha(&repo, &ShaSource::LocalHead).expect("HEAD must resolve");
    assert_eq!(oid, head);
}

#[test]
fn base_ref_falls_back_to_head_when_unresolvable() {
    let (dir, head) = repo_with_remote_tracking_ref();
    let repo = Repository::open(dir.path()).expect("reopen repo");
    let oid = resolve_sha(
        &repo,
        &ShaSource::BaseRef("origin/never-fetched".to_string()),
    )
    .expect("base ref must fall back to HEAD");
    assert_eq!(oid, head, "the fallback must be the HEAD commit");
}

#[test]
fn base_ref_head_is_the_head_commit() {
    let (dir, head) = repo_with_remote_tracking_ref();
    let repo = Repository::open(dir.path()).expect("reopen repo");
    for base_ref in ["HEAD", ""] {
        let oid = resolve_sha(&repo, &ShaSource::BaseRef(base_ref.to_string()))
            .expect("HEAD base ref must resolve");
        assert_eq!(oid, head);
    }
}

#[test]
fn resolve_at_reports_a_non_repository_path() {
    let dir = tempfile::tempdir().expect("temp dir");
    let result = resolve_sha_at(
        dir.path().to_str().expect("utf-8 path"),
        &ShaSource::LocalHead,
    );
    assert!(result.is_err(), "a non-repository path must be an error");
}

/// The pool stamps each entry with the SHA its worktree is checked out
/// at, read from the *worktree* directory. A linked worktree has a
/// `.git` FILE pointing into the main repo's `worktrees/<name>` admin
/// dir, so this pins that the resolver opens a linked worktree and reads
/// *its* HEAD, not the main repository's.
#[test]
fn local_head_resolves_inside_a_linked_worktree() {
    let (dir, head) = repo_with_remote_tracking_ref();
    let root = dir.path();
    let worktree_path = root.join("linked-wt");
    // `git worktree add` — the CLI is the only writer of worktree admin
    // metadata (ADR 0007's amendment).
    let output = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "--detach",
            worktree_path.to_str().expect("utf-8 path"),
            &head.to_string(),
        ])
        .current_dir(root)
        .output()
        .expect("git worktree add");
    assert!(
        output.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let oid = resolve_sha_at(
        worktree_path.to_str().expect("utf-8 path"),
        &ShaSource::LocalHead,
    )
    .expect("a linked worktree's HEAD must resolve");
    assert_eq!(oid, head);
}
