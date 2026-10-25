// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::env::test_helpers::{init_repo_with_commit, TestDir};
use std::fs;
use std::path::{Path, PathBuf};

/// A repository with the given committed files.
fn repo_with(files: &[(&str, &str)]) -> (TestDir, PathBuf) {
    let dir = TestDir::new("git_tree");
    init_repo_with_commit(dir.path(), files);
    let root = dir.path().to_path_buf();
    (dir, root)
}

/// A symlink entry is stored as a blob whose content is the link target; the
/// *mode* is what makes it a link rather than a regular file.
fn link_mode() -> i32 {
    git2::FileMode::Link.into()
}

/// `CLAUDE.md` on disk becomes an `AGENTS.md` symlink in the portability
/// commit — the link entry points at the sibling file by name.
#[test]
fn build_portability_commit_adds_the_agents_md_symlink() {
    let (_dir, root) = repo_with(&[("CLAUDE.md", "# context\n")]);

    let commit = build_portability_commit(&root, "portability", |_| None).expect("commit");

    assert_eq!(commit.added, vec!["AGENTS.md".to_string()]);
    assert!(!commit.is_empty());

    let repo = Repository::open(&root).expect("reopen repo");
    let reference = repo
        .find_reference("refs/heads/portability")
        .expect("branch");
    let tree = reference
        .peel_to_commit()
        .expect("commit")
        .tree()
        .expect("tree");
    let entry = tree.get_path(Path::new("AGENTS.md")).expect("entry");
    assert_eq!(
        entry.filemode(),
        link_mode(),
        "AGENTS.md must be a link, not a regular file"
    );
    let blob = repo.find_blob(entry.id()).expect("blob");
    assert_eq!(blob.content(), b"CLAUDE.md");
}

/// A repository with no `CLAUDE.md` has nothing to mirror, so the
/// portability commit adds nothing and creates no branch.
#[test]
fn build_portability_commit_does_nothing_without_a_claude_file() {
    let (_dir, root) = repo_with(&[("README.md", "readme\n")]);

    let commit = build_portability_commit(&root, "portability", |_| None).expect("commit");

    assert!(commit.is_empty());
    assert!(commit.added.is_empty());
    let repo = Repository::open(&root).expect("reopen repo");
    assert!(
        repo.find_reference("refs/heads/portability").is_err(),
        "an empty portability commit must not create a branch"
    );
}

/// An `AGENTS.md` that is already committed is left alone — the commit is
/// built off HEAD, so the existing entry is the one that counts.
#[test]
fn build_portability_commit_leaves_an_existing_agents_md_alone() {
    let (_dir, root) = repo_with(&[("CLAUDE.md", "# context\n"), ("AGENTS.md", "# agents\n")]);

    let commit = build_portability_commit(&root, "portability", |_| None).expect("commit");

    assert!(commit.is_empty());
}

/// The `.claude/skills` directory on disk becomes an `.agents/skills` link
/// whose target is relative to the `.agents` directory that holds it.
#[test]
fn build_portability_commit_adds_the_skills_symlink() {
    let (_dir, root) = repo_with(&[("README.md", "readme\n")]);
    fs::create_dir_all(root.join(".claude").join("skills")).expect("mkdir");

    let commit = build_portability_commit(&root, "portability", |_| None).expect("commit");

    assert_eq!(commit.added, vec![".agents/skills".to_string()]);

    let repo = Repository::open(&root).expect("reopen repo");
    let reference = repo
        .find_reference("refs/heads/portability")
        .expect("branch");
    let tree = reference
        .peel_to_commit()
        .expect("commit")
        .tree()
        .expect("tree");
    let agents = tree.get_path(Path::new(".agents")).expect("agents tree");
    let agents_tree = repo.find_tree(agents.id()).expect("agents tree");
    let skills = agents_tree
        .get_path(Path::new("skills"))
        .expect("skills entry");
    assert_eq!(skills.filemode(), link_mode());
    let blob = repo.find_blob(skills.id()).expect("blob");
    assert_eq!(blob.content(), b"../.claude/skills");
}

/// The `.gitignore` content is the caller's decision: the callback is handed
/// the committed content and its answer is what the commit carries.
#[test]
fn build_portability_commit_writes_the_gitignore_the_caller_asks_for() {
    let (_dir, root) = repo_with(&[(".gitignore", "node_modules\n")]);

    let commit = build_portability_commit(&root, "portability", |current| {
        let current = current.expect("the committed .gitignore");
        let mut amended = String::from_utf8_lossy(current).into_owned();
        amended.push_str("build/\n");
        Some(amended.into_bytes())
    })
    .expect("commit");

    assert_eq!(commit.added, vec![".gitignore".to_string()]);

    let repo = Repository::open(&root).expect("reopen repo");
    let reference = repo
        .find_reference("refs/heads/portability")
        .expect("branch");
    let tree = reference
        .peel_to_commit()
        .expect("commit")
        .tree()
        .expect("tree");
    let entry = tree.get_path(Path::new(".gitignore")).expect("entry");
    let blob = repo.find_blob(entry.id()).expect("blob");
    assert_eq!(blob.content(), b"node_modules\nbuild/\n");
}

/// The committed `.gitignore` is read back for the caller to compare
/// against.
#[test]
fn head_gitignore_content_returns_the_committed_content() {
    let (_dir, root) = repo_with(&[(".gitignore", "node_modules\n")]);

    assert_eq!(
        head_gitignore_content(&root).as_deref(),
        Some(&b"node_modules\n"[..])
    );
}

/// A repository with no committed `.gitignore` has none to report.
#[test]
fn head_gitignore_content_is_none_without_one() {
    let (_dir, root) = repo_with(&[("README.md", "readme\n")]);

    assert!(head_gitignore_content(&root).is_none());
}

/// The commit message names the symlinks and the regular-file updates
/// separately — `.gitignore` is an update, not a symlink (issue #1401).
#[test]
fn format_commit_message_names_symlinks_and_files_separately() {
    let symlink_only = format_commit_message(&["AGENTS.md".to_string()], &[]);
    assert!(symlink_only.contains("Adds AGENTS.md as git symlinks"));
    assert!(!symlink_only.contains(".gitignore"));

    let file_only = format_commit_message(&[], &[".gitignore".to_string()]);
    assert!(file_only.contains("updates .gitignore"));
    assert!(!file_only.contains("symlink"));

    let both = format_commit_message(
        &["AGENTS.md".to_string(), ".agents/skills".to_string()],
        &[".gitignore".to_string()],
    );
    assert!(both.contains("Adds AGENTS.md and .agents/skills as git symlinks"));
    assert!(both.contains("updates .gitignore"));
}
