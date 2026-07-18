//! Make a project's AI agent context portable across providers.
//!
//! Claude Code stores project context in `CLAUDE.md` and `.claude/skills/`.
//! Other agents Buildmesh can spawn — Codex, OpenCode, Antigravity — read the
//! cross-tool `AGENTS.md` open standard and a shared `.agents/skills/` directory.
//!
//! This module detects the Claude context and opens a PR adding `AGENTS.md` and
//! `.agents/skills` as git **symlinks** pointing back at the Claude files, so a
//! single source of truth serves every provider.
//!
//! The symlinks are written directly into the git object database (a blob whose
//! content is the link target, with filemode `120000`) and committed onto a fresh
//! branch off `HEAD`. The working tree and current branch are left untouched, and
//! no filesystem symlink is created — so this works on Windows regardless of the
//! Developer-Mode/admin privilege normally required to create symlinks there.

use crate::db;
use crate::env::to_host_path;
use crate::process_util::git_command;
use crate::services::github::{self, GitHubClient};
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::command;
use ts_rs::TS;

/// Wall-clock timeout for the `git push` shell-out in
/// [`create_ai_context_portability_pr_blocking`]. Push is network-bound
/// (remote `receive-pack`); 5 minutes mirrors [`crate::git::sync::MANUAL_FETCH_TIMEOUT`]
/// so a flaky network doesn't leak a blocking-pool thread (issue #762 review).
const PUSH_TIMEOUT: Duration = Duration::from_secs(300);

/// What AI-context files a project currently has, and what mirrors already exist.
/// Drives the Mesh Properties panel: enables the button only when there is
/// something to port that isn't already present.
///
/// Generated to src/types/generated/AiContextStatus.ts (issue #404).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "AiContextStatus.ts")]
pub struct AiContextStatus {
    /// `CLAUDE.md` exists at the repo root.
    pub claude_md_exists: bool,
    /// `AGENTS.md` already exists at the repo root.
    pub agents_md_exists: bool,
    /// `.claude/skills/` exists and is a directory.
    pub skills_dir_exists: bool,
    /// Number of skill directories inside `.claude/skills/`.
    #[ts(as = "i32")]
    pub skill_count: usize,
    /// `.agents/skills` already exists.
    pub agents_skills_exists: bool,
}

/// Detect the Claude AI context for a mesh path and report what can be ported.
///
/// Thin async wrapper; see [`crate::commands::git::get_git_branch_status`]
/// for the offload rationale. The filesystem probe (`is_dir`,
/// `read_dir`, `is_file`) on a large `.claude/skills/` tree can take a few
/// hundred ms; WSL UNC paths with paused VMs make that worse. Moving the
/// work to the blocking pool keeps a stalled probe from parking a Tauri
/// async worker.
#[command]
pub async fn detect_ai_context(mesh_path: String) -> Result<AiContextStatus, String> {
    crate::commands::run_blocking("detect_ai_context", move || {
        detect_ai_context_blocking(mesh_path)
    })
    .await
}

/// Sync core for [`detect_ai_context`].
pub(crate) fn detect_ai_context_blocking(mesh_path: String) -> Result<AiContextStatus, String> {
    let host = to_host_path(&mesh_path);
    let root = Path::new(&host);

    let skills_dir = root.join(".claude").join("skills");
    let skill_count = if skills_dir.is_dir() {
        std::fs::read_dir(&skills_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };

    Ok(AiContextStatus {
        claude_md_exists: root.join("CLAUDE.md").is_file(),
        agents_md_exists: root.join("AGENTS.md").exists(),
        skills_dir_exists: skills_dir.is_dir(),
        skill_count,
        agents_skills_exists: root.join(".agents").join("skills").exists(),
    })
}

/// Build the portability commit and open a PR for review.
///
/// Returns the GitHub PR URL on success.
#[command]
pub async fn create_ai_context_portability_pr(mesh_id: i64) -> Result<String, String> {
    // git object writes + `git push` + a GitHub REST round-trip — all blocking,
    // so run on the blocking pool rather than a Tauri async worker (see the
    // overnight-freeze investigation and [`crate::commands::run_blocking`]).
    crate::commands::run_blocking("create_ai_context_portability_pr", move || {
        create_ai_context_portability_pr_blocking(mesh_id)
    })
    .await
}

/// Sync core for [`create_ai_context_portability_pr`].
fn create_ai_context_portability_pr_blocking(mesh_id: i64) -> Result<String, String> {
    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let host_path = to_host_path(&mesh.path);
    let root = Path::new(&host_path);

    let repo = Repository::open(&host_path).map_err(|e| format!("git error: {}", e))?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let branch_name = format!("buildmesh/portable-ai-context-{}", ts);

    let added = build_portability_commit(&repo, root, &branch_name)?;
    if added.is_empty() {
        return Err(
            "Nothing to port: AGENTS.md and .agents/skills already exist, \
             or no CLAUDE.md / .claude/skills was found."
                .to_string(),
        );
    }

    // Push the new branch. Uses the user's configured git credentials via the CLI,
    // which is far more reliable than git2's manual credential callbacks.
    //
    // **Timeout (issue #762 review):** a hung `git push` (SSH key prompt,
    // paused WSL interop, half-open TLS) leaks a blocking-pool thread since
    // this core already runs on one. `run_command_with_timeout` kills the
    // child on the 5-min `MANUAL_FETCH_TIMEOUT` budget — same cap as `git fetch`
    // because push-side waits are dominated by the network + remote receive.
    let mut push_builder = git_command();
    push_builder
        .args(["push", "origin", &branch_name])
        .current_dir(&host_path);
    let push_out = crate::process_util::run_command_with_timeout(
        push_builder,
        "git push",
        PUSH_TIMEOUT,
    )
    .map_err(|e| format!("Failed to run git push: {}", e))?;
    if !push_out.status.success() {
        return Err(format!(
            "git push failed: {}",
            String::from_utf8_lossy(&push_out.stderr).trim()
        ));
    }

    // Open the PR against the repo's default branch (guaranteed to exist on the remote).
    let base = crate::commands::git::default_branch_from_repo(&repo);
    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(|u| u.to_string()));
    let (owner, repo_name) = remote_url
        .as_deref()
        .and_then(github::parse_owner_repo)
        .ok_or_else(|| "No GitHub origin remote configured".to_string())?;

    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    client
        .create_pull_request(
            &owner,
            &repo_name,
            "chore: make AI context portable across providers",
            &pr_body(&added),
            &branch_name,
            &base,
        )
        .map_err(|e| e.to_string())
}

/// Build a tree off `HEAD` adding the symlink entries that don't already exist,
/// then commit it onto `refs/heads/<branch_name>` without touching the working
/// tree or `HEAD`. Returns the list of paths added (empty if nothing to do).
///
/// Source presence is checked on the filesystem (what the user sees); existing
/// mirrors are checked in the `HEAD` tree (what's committed).
fn build_portability_commit(
    repo: &Repository,
    root: &Path,
    branch_name: &str,
) -> Result<Vec<String>, String> {
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

    let mut added: Vec<String> = Vec::new();

    // AGENTS.md -> CLAUDE.md (same directory, so the target is just "CLAUDE.md").
    let agents_md_present = root_builder.get("AGENTS.md").map_err(|e| e.to_string())?.is_some();
    if root.join("CLAUDE.md").is_file() && !agents_md_present {
        let blob = repo.blob(b"CLAUDE.md").map_err(|e| e.to_string())?;
        root_builder
            .insert("AGENTS.md", blob, link_mode)
            .map_err(|e| e.to_string())?;
        added.push("AGENTS.md".to_string());
    }

    // .agents/skills -> .claude/skills. The symlink lives in `.agents/`, so its
    // target relative to that directory is "../.claude/skills".
    if root.join(".claude").join("skills").is_dir() {
        // Re-use an existing `.agents` tree if the repo already has one.
        let existing_agents_id = match root_builder.get(".agents").map_err(|e| e.to_string())? {
            Some(e) if e.kind() == Some(git2::ObjectType::Tree) => Some(e.id()),
            _ => None,
        };
        let existing_agents_tree = existing_agents_id
            .map(|id| repo.find_tree(id))
            .transpose()
            .map_err(|e| e.to_string())?;

        let mut agents_builder = repo
            .treebuilder(existing_agents_tree.as_ref())
            .map_err(|e| e.to_string())?;
        let skills_present = agents_builder.get("skills").map_err(|e| e.to_string())?.is_some();
        if !skills_present {
            let blob = repo.blob(b"../.claude/skills").map_err(|e| e.to_string())?;
            agents_builder
                .insert("skills", blob, link_mode)
                .map_err(|e| e.to_string())?;
            let agents_tree_oid = agents_builder.write().map_err(|e| e.to_string())?;
            root_builder
                .insert(".agents", agents_tree_oid, tree_mode)
                .map_err(|e| e.to_string())?;
            added.push(".agents/skills".to_string());
        }
    }

    if added.is_empty() {
        return Ok(added);
    }

    let new_tree_oid = root_builder.write().map_err(|e| e.to_string())?;
    let new_tree = repo.find_tree(new_tree_oid).map_err(|e| e.to_string())?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("buildmesh", "buildmesh@local"))
        .map_err(|e| e.to_string())?;

    let commit_msg = format!(
        "chore: make AI context portable across providers\n\n\
         Adds {} as git symlinks so non-Claude agents read the same context.",
        added.join(" and ")
    );
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

    Ok(added)
}

fn pr_body(added: &[String]) -> String {
    let bullets: String = added
        .iter()
        .map(|a| match a.as_str() {
            "AGENTS.md" => {
                "- `AGENTS.md` → `CLAUDE.md` (the cross-tool open standard read by Codex, OpenCode and Antigravity)"
            }
            ".agents/skills" => {
                "- `.agents/skills` → `.claude/skills` (shared Agent Skills directory; Antigravity reads `.agents/skills` natively)"
            }
            _ => "",
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Makes this project's AI agent context portable across providers.\n\n\
         Buildmesh added the following as **symlinks** so non-Claude agents \
         (Codex, OpenCode, Antigravity) read the same context Claude Code uses:\n\n\
         {bullets}\n\n\
         ⚠️ These are git symlinks. Checked out on **Windows without Developer Mode**, \
         git materialises them as plain text files containing the target path rather than \
         working symlinks. On macOS, Linux, or Windows with Developer Mode they resolve correctly.\n\n\
         🤖 Generated with [Buildmesh]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    struct TempRepo(std::path::PathBuf);
    impl TempRepo {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            let tmp = std::env::temp_dir().join(format!("buildmesh_aictx_test_{}", id));
            let _ = fs::remove_dir_all(&tmp);
            Self(tmp)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Init a repo with one commit containing the given relative files.
    fn init_repo(path: &Path, files: &[(&str, &str)]) -> git2::Repository {
        fs::create_dir_all(path).unwrap();
        let repo = git2::Repository::init(path).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();

        let mut index = repo.index().unwrap();
        for (name, content) in files {
            let full = path.join(name);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(&full, content).unwrap();
            index.add_path(Path::new(name)).unwrap();
        }
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        {
            let tree = repo.find_tree(tree_oid).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    fn branch_tree<'a>(repo: &'a git2::Repository, branch: &str) -> git2::Tree<'a> {
        repo.find_reference(&format!("refs/heads/{}", branch))
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap()
    }

    #[test]
    fn adds_both_symlinks_with_link_filemode() {
        let tr = TempRepo::new();
        let repo = init_repo(
            tr.path(),
            &[
                ("CLAUDE.md", "# context"),
                (".claude/skills/foo/SKILL.md", "---\nname: foo\n---\n"),
            ],
        );

        let head_before = repo.head().unwrap().peel_to_commit().unwrap().id();

        let added = build_portability_commit(&repo, tr.path(), "test-branch").unwrap();
        assert_eq!(added, vec!["AGENTS.md".to_string(), ".agents/skills".to_string()]);

        let tree = branch_tree(&repo, "test-branch");

        let agents_md = tree.get_name("AGENTS.md").unwrap();
        assert_eq!(agents_md.filemode(), 0o120000);
        assert_eq!(repo.find_blob(agents_md.id()).unwrap().content(), b"CLAUDE.md");

        let agents_dir = tree.get_name(".agents").unwrap();
        let agents_tree = repo.find_tree(agents_dir.id()).unwrap();
        let skills = agents_tree.get_name("skills").unwrap();
        assert_eq!(skills.filemode(), 0o120000);
        assert_eq!(
            repo.find_blob(skills.id()).unwrap().content(),
            b"../.claude/skills"
        );

        // HEAD and working tree untouched.
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().id(), head_before);
        assert!(!tr.path().join("AGENTS.md").exists());
        assert!(!tr.path().join(".agents").exists());
    }

    #[test]
    fn idempotent_when_mirrors_already_committed() {
        let tr = TempRepo::new();
        let repo = init_repo(
            tr.path(),
            &[
                ("CLAUDE.md", "# context"),
                ("AGENTS.md", "CLAUDE.md"),
                (".claude/skills/foo/SKILL.md", "x"),
                (".agents/skills/foo/SKILL.md", "x"),
            ],
        );

        let added = build_portability_commit(&repo, tr.path(), "test-branch").unwrap();
        assert!(added.is_empty(), "should add nothing when mirrors exist");
        // No branch should have been created.
        assert!(repo.find_reference("refs/heads/test-branch").is_err());
    }

    #[test]
    fn adds_only_skills_when_agents_md_exists() {
        let tr = TempRepo::new();
        let repo = init_repo(
            tr.path(),
            &[
                ("CLAUDE.md", "# context"),
                ("AGENTS.md", "CLAUDE.md"),
                (".claude/skills/foo/SKILL.md", "x"),
            ],
        );

        let added = build_portability_commit(&repo, tr.path(), "test-branch").unwrap();
        assert_eq!(added, vec![".agents/skills".to_string()]);
    }

    #[test]
    fn detect_reports_skill_count() {
        let tr = TempRepo::new();
        init_repo(
            tr.path(),
            &[
                ("CLAUDE.md", "# context"),
                (".claude/skills/a/SKILL.md", "x"),
                (".claude/skills/b/SKILL.md", "x"),
            ],
        );

        let status = detect_ai_context_blocking(tr.path().to_string_lossy().to_string()).unwrap();
        assert!(status.claude_md_exists);
        assert!(!status.agents_md_exists);
        assert!(status.skills_dir_exists);
        assert_eq!(status.skill_count, 2);
        assert!(!status.agents_skills_exists);
    }
}
