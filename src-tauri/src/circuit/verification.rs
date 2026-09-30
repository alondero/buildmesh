//! Observable git publication prerequisites and correction prompts for Circuit steps.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WrapupState {
    pub dirty: bool,
    /// Branch pushed with an up-to-date upstream (upstream exists, 0 ahead).
    pub pushed: bool,
    /// The branch currently checked out in the inspected worktree.
    pub branch: Option<String>,
    /// Open PR URL for the branch, if any.
    pub pr_url: Option<String>,
    /// The same PR's number — persisted to the ledger on completion so the
    /// merged-PR auto-close sweep can check it without re-deriving the branch.
    pub pr_number: Option<i64>,
    /// Does the mesh's policy require a PR (`action_on_success != "none"`)?
    pub pr_required: bool,
    /// The node's worktree could not be opened as a git repository — the
    /// dirty/pushed/PR fields are unknowable, and the correction must say so
    /// instead of fabricating "uncommitted changes" (2026-07-17 gh252 run:
    /// a broken worktree produced three invented reasons and sent the agent
    /// chasing state that was never wrong).
    pub repo_error: Option<String>,
}

pub(crate) fn wrapup_reasons(state: &WrapupState) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(err) = &state.repo_error {
        // Unopenable repo: the git-state checks below would all be
        // fabrications. Report the one true failure so the agent repairs the
        // worktree the harness is actually looking at.
        reasons.push(err.clone());
    } else {
        if state.dirty {
            reasons.push("the worktree still has uncommitted changes".to_string());
        }
        if !state.pushed {
            reasons.push("the branch has not been pushed to origin (or has unpushed commits)".to_string());
        }
        if state.pr_required && state.pr_url.is_none() {
            reasons.push("no open pull request exists for the branch".to_string());
        }
    }
    reasons
}

const CORRECTION_TAIL_CHARS: usize = 1_200;

/// The correction prompt written back into the PTY (#485 AC wording),
/// carrying the failure reasons plus the recent terminal tail.
pub(crate) fn correction_prompt(reasons: &[String], recent_output: &str) -> String {
    let tail = if recent_output.len() > CORRECTION_TAIL_CHARS {
        let mut start = recent_output.len() - CORRECTION_TAIL_CHARS;
        while !recent_output.is_char_boundary(start) {
            start += 1;
        }
        &recent_output[start..]
    } else {
        recent_output
    };
    let mut prompt = format!(
        "The automated wrap-up verification failed. Please fix this: {}. \
         Then complete the remaining wrap-up steps (commit, push, PR) and report the result.",
        reasons.join("; ")
    );
    if !tail.trim().is_empty() {
        prompt.push_str("\n\nRecent terminal output at the time of the check:\n");
        prompt.push_str(tail);
    }
    prompt
}

pub(crate) fn observe_wrapup_git_state(
    node: &crate::models::AgentNode,
) -> WrapupState {
    // `node_working_path` resolves Worktree and Root Nodes alike (host path +
    // env), so the self-heal below covers both; on a Root Node the sanitize
    // is a no-op (`.git` is a directory, not a gitlink).
    let resolved = crate::env::node_working_path(node);
    let host_path = resolved.host_path.clone();

    // Self-heal before giving up: an MSYS-flavoured git leaves Git-Bash-style
    // `/f/...` paths in the worktree link files that the agent's CLI reads
    // fine but libgit2 reports as NotFound (the 2026-07-17 gh252 incident —
    // the agent had to run `git worktree repair` itself). Sanitize both link
    // sides and retry once, so a format-only mismatch never reaches the
    // repo_error path and never costs a correction attempt.
    let opened = git2::Repository::open(&host_path).or_else(|first_err| {
        tracing::info!(
            "circuit verification({}): open failed ({}); sanitizing worktree links and retrying",
            node.id,
            first_err
        );
        if let Err(e) = crate::git::worktree::sanitize_git_worktree(&host_path, resolved.env_type)
        {
            tracing::warn!("circuit verification({}): sanitize failed: {}", node.id, e);
        }
        git2::Repository::open(&host_path)
    });

    let (dirty, branch, pushed, repo_error) = match opened {
        Ok(repo) => {
            let dirty = crate::git::primitives::is_dirty(&repo).unwrap_or(true);
            let branch = crate::git::primitives::head_branch_name(&repo);
            let pushed = branch
                .as_deref()
                .and_then(|b| {
                    let local = repo.find_branch(b, git2::BranchType::Local).ok()?;
                    let upstream = local.upstream().ok()?;
                    let local_oid = local.get().target()?;
                    let up_oid = upstream.get().target()?;
                    let (ahead, _behind) =
                        crate::git::primitives::ahead_behind(&repo, local_oid, up_oid).ok()?;
                    Some(ahead == 0)
                })
                .unwrap_or(false);
            (dirty, branch, pushed, None)
        }
        Err(e) => {
            tracing::warn!(
                "circuit verification({}): could not open repo at {}: {}",
                node.id,
                host_path,
                e
            );
            // Name the exact path the harness inspects — without it the agent
            // has no way to know where the check is looking and starts
            // guessing (renaming branches, recreating worktrees elsewhere).
            let repo_error = format!(
                "the verification could not open the node's worktree at {} as a git repository ({}) — \
                 repair or recreate the worktree at that exact path and do your wrap-up (commit, push, PR) from it",
                host_path, e
            );
            (true, None, false, Some(repo_error))
        }
    };

    WrapupState {
        dirty, pushed, branch, pr_url: None, pr_number: None, pr_required: false, repo_error,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SessionStatus;
    fn wrapup_test_node() -> crate::models::AgentNode {
        crate::models::AgentNode {
            launch_configuration: None,
            id: 1,
            mesh_id: 1,
            name: "gh1-missing".to_string(),
            path: std::env::temp_dir()
                .join("bm-observe-missing-mesh")
                .to_string_lossy()
                .to_string(),
            branch: "main".to_string(),
            env: crate::models::EnvType::default(),
            provider: "anthropic".to_string(),
            status: SessionStatus::Running,
            cli_session_id: None,
            worktree_name: Some("gh1-missing".to_string()),
            use_worktree: true,
            // Required by the full-literal `AgentNode { ... }` initializer
            // (wayfinder #982 / ticket #984). `is_pinned` is a UI-toggle
            // field unrelated to this test's repo-open failure path;
            // `false` matches the column default for a fresh node.
            is_pinned: false,
            source_issue: Some(1),
            source_pr: None,
            head_repo_owner: None,
            head_repo_clone_url: None,
            source_pr_pinned_sha: None,
            lifecycle: None,
            signal_health: None,
            position: 0,
            created_at: chrono::Utc::now(),
            worktree_path: None,
        }
    }

    #[test]
    fn observe_wrapup_git_state_uses_actual_branch_without_github_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("test", "test@example.com").unwrap();
        let commit = repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[]).unwrap();
        repo.branch("renamed-implementation", &repo.find_commit(commit).unwrap(), false).unwrap();
        repo.set_head("refs/heads/renamed-implementation").unwrap();
        repo.remote("origin", "https://github.com/example/repo.git").unwrap();
        repo.reference("refs/remotes/origin/renamed-implementation", commit, true, "test").unwrap();
        repo.find_branch("renamed-implementation", git2::BranchType::Local).unwrap()
            .set_upstream(Some("origin/renamed-implementation")).unwrap();
        let worktree_path = dir.path().join("original-worktree-name");
        let branch_ref = repo.find_reference("refs/heads/renamed-implementation").unwrap();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&branch_ref));
        // Release the branch from the main checkout before attaching it.
        repo.set_head_detached(commit).unwrap();
        repo.worktree("original-worktree-name", &worktree_path, Some(&options)).unwrap();
        let mut node = wrapup_test_node();
        node.path = dir.path().to_string_lossy().into_owned();
        node.worktree_name = Some("original-worktree-name".into());
        node.worktree_path = Some(worktree_path.to_string_lossy().into_owned());
        let state = observe_wrapup_git_state(&node);
        assert_eq!(state.branch.as_deref(), Some("renamed-implementation"));
        assert_ne!(state.branch, node.worktree_name);
        assert!(state.pushed);
        assert!(wrapup_reasons(&state).is_empty());
        assert!(state.pr_url.is_none());
    }

}
