//! Tree and blob plumbing (issue #2526).
//!
//! The AI-context portability commit builds a tree off `HEAD` — symlink
//! entries, an amended `.gitignore` — and commits it onto a branch without
//! touching the working tree. That is tree/blob work rather than a state
//! read, so it has its own module instead of riding on [`super::state`].
//!
//! The `.gitignore` block itself stays with the command layer: this module
//! asks the caller what the new content should be, so the canonical block
//! keeps one home.

use std::path::Path;

use git2::{ObjectType, Repository, TreeBuilder};

/// What a portability commit added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortabilityCommit {
    /// The paths the commit added — symlinks first, then files.
    pub added: Vec<String>,
}

impl PortabilityCommit {
    /// True when the commit adds nothing — the repository already carries
    /// every mirror, so there is no portability work to do.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
    }
}

/// Build the tree off `HEAD` adding the symlink entries that don't already
/// exist, then commit it onto `refs/heads/<branch_name>` without touching the
/// working tree or `HEAD`.
///
/// `gitignore_update` is asked what the new `.gitignore` content should be.
/// It receives the committed `.gitignore` — `None` when the tree has none —
/// and returns the replacement content, or `None` to leave the file alone.
///
/// Source presence is checked on the filesystem (what the user sees); existing
/// mirrors are checked in the `HEAD` tree (what's committed).
pub fn build_portability_commit<F>(
    root: &Path,
    branch_name: &str,
    gitignore_update: F,
) -> Result<PortabilityCommit, String>
where
    F: Fn(Option<&[u8]>) -> Option<Vec<u8>>,
{
    let repo = Repository::open(root).map_err(|e| format!("git error: {}", e))?;
    let head_commit = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| format!("cannot read HEAD commit: {}", e))?;
    let head_tree = head_commit.tree().map_err(|e| e.to_string())?;

    let mut root_builder = repo
        .treebuilder(Some(&head_tree))
        .map_err(|e| e.to_string())?;
    let link_mode: i32 = git2::FileMode::Link.into();
    let tree_mode: i32 = git2::FileMode::Tree.into();

    let mut symlinks_added: Vec<String> = Vec::new();
    let mut files_added: Vec<String> = Vec::new();

    // AGENTS.md -> CLAUDE.md (same directory, so the target is just "CLAUDE.md").
    let agents_md_present = root_builder
        .get("AGENTS.md")
        .map_err(|e| e.to_string())?
        .is_some();
    if root.join("CLAUDE.md").is_file() && !agents_md_present {
        let blob = repo.blob(b"CLAUDE.md").map_err(|e| e.to_string())?;
        root_builder
            .insert("AGENTS.md", blob, link_mode)
            .map_err(|e| e.to_string())?;
        symlinks_added.push("AGENTS.md".to_string());
    }

    // .agents/skills -> .claude/skills. The symlink lives in `.agents/`, so its
    // target relative to that directory is "../.claude/skills".
    if root.join(".claude").join("skills").is_dir() {
        // Re-use an existing `.agents` tree if the repo already has one.
        let existing_agents_id = match root_builder.get(".agents").map_err(|e| e.to_string())? {
            Some(e) if e.kind() == Some(ObjectType::Tree) => Some(e.id()),
            _ => None,
        };
        let existing_agents_tree = existing_agents_id
            .map(|id| repo.find_tree(id))
            .transpose()
            .map_err(|e| e.to_string())?;

        let mut agents_builder = repo
            .treebuilder(existing_agents_tree.as_ref())
            .map_err(|e| e.to_string())?;
        let skills_present = agents_builder
            .get("skills")
            .map_err(|e| e.to_string())?
            .is_some();
        if !skills_present {
            let blob = repo.blob(b"../.claude/skills").map_err(|e| e.to_string())?;
            agents_builder
                .insert("skills", blob, link_mode)
                .map_err(|e| e.to_string())?;
            let agents_tree_oid = agents_builder.write().map_err(|e| e.to_string())?;
            root_builder
                .insert(".agents", agents_tree_oid, tree_mode)
                .map_err(|e| e.to_string())?;
            symlinks_added.push(".agents/skills".to_string());
        }
    }

    // .gitignore: amend it when the caller says it needs amending. Read
    // HEAD's blob — that's what the new tree is built off of — so the result
    // is consistent with the rest of the commit and idempotent against the
    // user's previous commits.
    //
    // The decision (and the blob content to write) is computed up front so
    // the immutable borrow of `root_builder` from `.get()` drops before we
    // call the mutable `.insert()`.
    if let Some(new_gitignore_bytes) =
        gitignore_update_bytes(&repo, &root_builder, &gitignore_update)?
    {
        let new_blob = repo.blob(&new_gitignore_bytes).map_err(|e| e.to_string())?;
        root_builder
            .insert(".gitignore", new_blob, git2::FileMode::Blob.into())
            .map_err(|e| e.to_string())?;
        files_added.push(".gitignore".to_string());
    }

    if symlinks_added.is_empty() && files_added.is_empty() {
        return Ok(PortabilityCommit::default());
    }

    let new_tree_oid = root_builder.write().map_err(|e| e.to_string())?;
    let new_tree = repo.find_tree(new_tree_oid).map_err(|e| e.to_string())?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("buildmesh", "buildmesh@local"))
        .map_err(|e| e.to_string())?;

    let commit_msg = format_commit_message(&symlinks_added, &files_added);
    let branch_ref = format!("refs/heads/{}", branch_name);
    repo.commit(
        Some(&branch_ref),
        &sig,
        &sig,
        &commit_msg,
        &new_tree,
        &[&head_commit],
    )
    .map_err(|e| format!("failed to create commit: {}", e))?;

    // Combine symlinks + files for the caller — the PR body uses the same
    // list to decide which bullets to render.
    let mut added: Vec<String> = Vec::with_capacity(symlinks_added.len() + files_added.len());
    added.extend(symlinks_added);
    added.extend(files_added);
    Ok(PortabilityCommit { added })
}

/// The committed `.gitignore` of the repository at `root`.
///
/// `None` when the repository cannot be opened, `HEAD` cannot be resolved, or
/// the tree has no `.gitignore` blob. The caller decides what the content
/// means — the canonical block lives with the command layer.
pub fn head_gitignore_content(root: &Path) -> Option<Vec<u8>> {
    let repo = Repository::open(root).ok()?;
    let head_commit = repo.head().and_then(|h| h.peel_to_commit()).ok()?;
    let head_tree = head_commit.tree().ok()?;
    let entry = head_tree.get_name(".gitignore")?;
    if entry.kind() != Some(ObjectType::Blob) {
        return None;
    }
    let blob = repo.find_blob(entry.id()).ok()?;
    Some(blob.content().to_vec())
}

/// The new `.gitignore` content, if the caller wants one.
///
/// The decision (and the blob content to write) is computed up front so the
/// immutable borrow of `root_builder` from `.get()` drops before the caller
/// calls the mutable `.insert()`.
fn gitignore_update_bytes<F>(
    repo: &Repository,
    root_builder: &TreeBuilder<'_>,
    update: &F,
) -> Result<Option<Vec<u8>>, String>
where
    F: Fn(Option<&[u8]>) -> Option<Vec<u8>>,
{
    match root_builder.get(".gitignore").map_err(|e| e.to_string())? {
        // No `.gitignore` in HEAD — the caller decides what to create.
        None => Ok(update(None)),
        Some(entry) if entry.kind() == Some(ObjectType::Blob) => {
            let blob = repo.find_blob(entry.id()).map_err(|e| e.to_string())?;
            Ok(update(Some(blob.content())))
        }
        // `.gitignore` is a submodule or symlink — leave it alone rather
        // than guess at its content.
        Some(_) => Ok(None),
    }
}

/// Compose a commit message that doesn't claim `.gitignore` is a git
/// symlink (issue #1401 review). Symlinks (`AGENTS.md`, `.agents/skills`)
/// are listed in their own phrase; regular-file updates (`.gitignore`)
/// get their own. When only one category is present, only that category's
/// phrase is rendered.
fn format_commit_message(symlinks: &[String], files: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !symlinks.is_empty() {
        parts.push(format!("Adds {} as git symlinks", symlinks.join(" and ")));
    }
    if !files.is_empty() {
        parts.push(format!("updates {}", files.join(" and ")));
    }
    let summary = if parts.len() == 1 {
        parts.remove(0)
    } else {
        parts.join(" and ")
    };
    format!(
        "chore: make AI context portable across providers\n\n\
         {summary} so non-Claude agents read the same context."
    )
}
