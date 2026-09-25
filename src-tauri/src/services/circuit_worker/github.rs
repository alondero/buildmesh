//! GitHub effects for the circuit worker (issue #1660).
//! A new GitHub action kind is added here, not in the observe-step-commit loop.

use crate::autopilot::circuit::model::CircuitNodeKind;
use crate::autopilot::circuit::stepper::{CircuitEvent, RunView};
use crate::db;


/// Determine the target (issue vs PR number) for a GitHub action.
/// If the action is CloseIssue, it explicitly requires an issue trigger.
/// If this node has an upstream OpenPr node in its lineage, it targets that PR (pr.number).
/// Otherwise, it falls back to issue.number if present, then pr.number.
pub(super) fn determine_github_target(
    view: &RunView,
    node_id: &str,
    action: crate::autopilot::circuit::model::GithubActionKind,
) -> Result<(&'static str, i64), String> {
    use crate::autopilot::circuit::model::GithubActionKind;
    if action == GithubActionKind::CloseIssue {
        let num = view
            .context
            .get("issue.number")
            .and_then(|n| n.parse::<i64>().ok())
            .ok_or_else(|| {
                "CloseIssue requires an issue-triggered run with issue.number".to_string()
            })?;
        return Ok(("issue", num));
    }

    let has_upstream_open_pr = view.has_upstream_node_of_kind(node_id, |kind| {
        matches!(
            kind,
            CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                ..
            }
        )
    });

    if has_upstream_open_pr {
        if let Some(pr_num) = view
            .context
            .get("pr.number")
            .and_then(|n| n.parse::<i64>().ok())
        {
            return Ok(("pr", pr_num));
        }
    }

    if let Some(issue_num) = view
        .context
        .get("issue.number")
        .and_then(|n| n.parse::<i64>().ok())
    {
        Ok(("issue", issue_num))
    } else if let Some(pr_num) = view
        .context
        .get("pr.number")
        .and_then(|n| n.parse::<i64>().ok())
    {
        Ok(("pr", pr_num))
    } else {
        Err("GitHub action has no issue/pr context — the circuit needs a GitHub trigger upstream of this node".to_string())
    }
}

/// Reconcile the implementation branch with GitHub before emitting the durable
/// result. Inject the external observations so replay and lookup failures can
/// be exercised without a process-wide database or GitHub writes.
#[cfg(test)]
pub(super) fn ensure_open_pr(
    view: &RunView,
    node_id: &str,
    policy: Option<crate::autopilot::circuit::model::OpenPrPolicy>,
    observe: impl FnOnce(i64) -> Result<crate::autopilot::pipeline::WrapupState, String>,
    find: impl FnOnce(&str) -> Result<Option<crate::services::github::PullRequest>, String>,
    create: impl FnOnce(&str, &str) -> Result<crate::services::github::PullRequest, String>,
) -> Result<CircuitEvent, String> {
    ensure_open_pr_with_target(view, node_id, policy, observe, |_| Ok(()), find, create)
}

pub(super) fn ensure_open_pr_with_target(
    view: &RunView,
    node_id: &str,
    policy: Option<crate::autopilot::circuit::model::OpenPrPolicy>,
    observe: impl FnOnce(i64) -> Result<crate::autopilot::pipeline::WrapupState, String>,
    record_target: impl FnOnce(&str) -> Result<(), String>,
    find: impl FnOnce(&str) -> Result<Option<crate::services::github::PullRequest>, String>,
    create: impl FnOnce(&str, &str) -> Result<crate::services::github::PullRequest, String>,
) -> Result<CircuitEvent, String> {
    let agent_node_id = view
        .resolve_open_pr_agent(node_id)
        .ok_or_else(|| "OpenPr requires a spawned agent earlier in this run".to_string())?;
    let wrapup = observe(agent_node_id)?;
    let reasons = crate::autopilot::pipeline::wrapup_reasons(&wrapup);
    if !reasons.is_empty() {
        return Err(format!(
            "autopilot wrap-up verification failed: {}",
            reasons.join("; ")
        ));
    }
    let head = wrapup
        .branch
        .clone()
        .ok_or_else(|| "the implementation worktree has no checked-out branch".to_string())?;
    record_target(&head)?;
    let title = view
        .context
        .get("issue.title")
        .or_else(|| view.context.get("pr.title"))
        .unwrap_or("Circuit run")
        .to_string();
    // A replay after GitHub accepted creation but before the ledger commit
    // must discover that PR, never create a second one.
    let pr = match find(&head)? {
        Some(pr) => pr,
        None if policy.is_some_and(|policy| policy.requires_existing()) => {
            return Err(
                "the implementation agent did not raise an open pull request for its branch"
                    .to_string(),
            );
        }
        None => create(&head, &title)?,
    };
    let head_ref = if pr.head_ref.trim().is_empty() {
        head
    } else {
        pr.head_ref
    };
    let title = if pr.title.trim().is_empty() {
        title
    } else {
        pr.title
    };
    Ok(CircuitEvent::GithubActionResult {
        node_id: node_id.to_string(),
        success: true,
        pr_number: Some(pr.number),
        pr_url: Some(pr.html_url),
        pr_head_ref: Some(head_ref),
        pr_title: if title.is_empty() { None } else { Some(title) },
        error: None,
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct OpenPrEffectTarget {
    owner: String,
    repo: String,
    head: String,
}

fn recheck_open_pr_target(
    target: &OpenPrEffectTarget,
    find: impl FnOnce(&str, &str, &str) -> Result<Option<crate::services::github::PullRequest>, String>,
) -> Result<crate::services::github::PullRequest, String> {
    if target.owner.trim().is_empty()
        || target.repo.trim().is_empty()
        || target.head.trim().is_empty()
    {
        return Err("The saved OpenPr target is incomplete; no GitHub request was made.".into());
    }
    let pull_request = find(&target.owner, &target.repo, &target.head)?;
    let pull_request = pull_request.ok_or_else(|| {
        format!(
            "No open pull request was found for {}/{} branch {}; this recheck did not create one.",
            target.owner, target.repo, target.head
        )
    })?;
    if !pull_request.head_ref.trim().is_empty() && pull_request.head_ref != target.head {
        return Err(format!(
            "GitHub returned branch {} while rechecking {}; the result remains unverified.",
            pull_request.head_ref, target.head
        ));
    }
    Ok(pull_request)
}

pub(super) fn reconcile_open_pr_effect(
    active: &db::ActiveCircuitRun,
    view: &RunView,
    node_id: &str,
) -> Result<CircuitEvent, String> {
    use crate::services::github::GitHubClient;

    let attempt = view.step(node_id).map_or(1, |step| step.attempt);
    let detail = db::circuit::evidence::latest_effect_target(active.run.id, node_id, attempt)?
        .ok_or_else(|| {
            "No durable OpenPr target is recorded for this attempt; no GitHub request was made."
                .to_string()
        })?;
    let target: OpenPrEffectTarget = serde_json::from_str(&detail)
        .map_err(|_| "The saved OpenPr target cannot be read; no GitHub request was made.".to_string())?;
    let client = GitHubClient::new().map_err(|error| error.to_string())?;
    let pull_request = recheck_open_pr_target(&target, |owner, repo, head| {
        client
            .find_open_pr_for_branch(owner, repo, head)
            .map_err(|error| error.to_string())
    })?;
    let title = pull_request.title.trim();
    Ok(CircuitEvent::GithubActionResult {
        node_id: node_id.to_string(),
        success: true,
        pr_number: Some(pull_request.number),
        pr_url: Some(pull_request.html_url),
        pr_head_ref: Some(if pull_request.head_ref.trim().is_empty() {
            target.head
        } else {
            pull_request.head_ref
        }),
        pr_title: (!title.is_empty()).then(|| title.to_string()),
        error: None,
    })
}

/// Perform one `CallGithub` effect (milestone 3, issue #1208): resolve
/// the target repo from the mesh's `origin`, execute the mutation through
/// the shared [`crate::services::github::GitHubClient`] seam, and advance
/// the stepper with the result so context updates (e.g. `pr.*`) commit atomically
/// before downstream nodes cascade.
pub(super) fn call_github_effect(
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    node_id: &str,
    action: crate::autopilot::circuit::model::GithubActionKind,
    label: Option<&str>,
    comment: Option<&str>,
) -> Result<CircuitEvent, String> {
    use crate::autopilot::circuit::model::GithubActionKind;
    use crate::services::github::GitHubClient;

    if action == GithubActionKind::OpenPr
        && crate::autopilot::circuit::stepper::resolve_upstream_spawn_agent(
            &view.graph,
            &view.steps,
            node_id,
        )
        .is_none()
    {
        return Err(
            "OpenPr cannot recover: its upstream spawned agent association is missing".into(),
        );
    }

    let mesh = db::get_mesh_by_id(active.run.mesh_id).map_err(|e| e.to_string())?;
    let (owner, repo) = crate::commands::pr::resolve_github_owner_repo(&mesh)?;
    let client = GitHubClient::new().map_err(|e| e.to_string())?;
    let resolved_comment = comment.map(|c| view.context.resolve(c));
    let open_pr_policy = match view.graph.node(node_id).map(|node| &node.kind) {
        Some(CircuitNodeKind::GithubAction { open_pr_policy, .. }) => *open_pr_policy,
        _ => None,
    };

    let action_res: Result<CircuitEvent, String> = (|| match action {
        GithubActionKind::AddLabel => {
            let target = determine_github_target(view, node_id, action)?;
            let label = label.ok_or_else(|| "AddLabel requires a label".to_string())?;
            client
                .add_issue_label(&owner, &repo, target.1, &view.context.resolve(label))
                .map_err(|e| e.to_string())?;
            Ok(CircuitEvent::GithubActionResult {
                node_id: node_id.to_string(),
                success: true,
                pr_number: None,
                pr_url: None,
                pr_head_ref: None,
                pr_title: None,
                error: None,
            })
        }
        GithubActionKind::RemoveLabel => {
            let target = determine_github_target(view, node_id, action)?;
            let label = label.ok_or_else(|| "RemoveLabel requires a label".to_string())?;
            client
                .remove_issue_label(&owner, &repo, target.1, &view.context.resolve(label))
                .map_err(|e| e.to_string())?;
            Ok(CircuitEvent::GithubActionResult {
                node_id: node_id.to_string(),
                success: true,
                pr_number: None,
                pr_url: None,
                pr_head_ref: None,
                pr_title: None,
                error: None,
            })
        }
        GithubActionKind::PostComment => {
            let target = determine_github_target(view, node_id, action)?;
            let body = resolved_comment
                .filter(|c| !c.trim().is_empty())
                .ok_or_else(|| "PostComment requires a non-empty comment template".to_string())?;
            client
                .add_issue_comment(&owner, &repo, target.1, &body)
                .map_err(|e| e.to_string())?;
            Ok(CircuitEvent::GithubActionResult {
                node_id: node_id.to_string(),
                success: true,
                pr_number: None,
                pr_url: None,
                pr_head_ref: None,
                pr_title: None,
                error: None,
            })
        }
        GithubActionKind::CloseIssue => {
            let target = determine_github_target(view, node_id, action)?;
            if target.0 != "issue" {
                return Err("CloseIssue requires an issue-triggered run".to_string());
            }
            client
                .close_issue(&owner, &repo, target.1)
                .map_err(|e| e.to_string())?;
            Ok(CircuitEvent::GithubActionResult {
                node_id: node_id.to_string(),
                success: true,
                pr_number: None,
                pr_url: None,
                pr_head_ref: None,
                pr_title: None,
                error: None,
            })
        }
        GithubActionKind::OpenPr => {
            if open_pr_policy.is_some_and(|policy| policy.requires_existing())
                && crate::services::autopilot::configured_action_on_success(active.run.mesh_id)
                    == "none"
            {
                return Err("this OpenPr action requires a pull-request wrap-up policy".to_string());
            }
            let body = resolved_comment.unwrap_or_default();
            let mut target_revision = None;
            let result = ensure_open_pr_with_target(
                view,
                node_id,
                open_pr_policy,
                |agent_node_id| {
                    let agent_node =
                        db::get_agent_node_by_id(agent_node_id).map_err(|e| e.to_string())?;
                    if !agent_node.use_worktree {
                        return Err(
                            "OpenPr requires a worktree-backed agent (its commits have no branch)"
                                .to_string(),
                        );
                    }
                    Ok(crate::autopilot::pipeline::observe_wrapup_git_state(
                        &agent_node,
                    ))
                },
                |head| {
                    let target = OpenPrEffectTarget {
                        owner: owner.clone(),
                        repo: repo.clone(),
                        head: head.to_string(),
                    };
                    let detail = serde_json::to_string(&target).map_err(|error| error.to_string())?;
                    target_revision = Some(db::circuit::evidence::record_effect_target(
                        active.run.id,
                        node_id,
                        view.step(node_id).map_or(1, |step| step.attempt),
                        &detail,
                    )?);
                    Ok(())
                },
                |head| {
                    client.find_open_pr_for_branch(&owner, &repo, head)
                        .map_err(|e| format!("could not verify the pull request for {owner}/{repo} branch {head}: {e}"))
                },
                |head, title| {
                    let base =
                        crate::commands::git::get_default_branch_blocking(mesh.path.clone())?;
                    // Idempotent create (issue #771): a slow POST that
                    // timed out client-side may have created the PR
                    // server-side, so a replay would 422. The idempotent
                    // helper recovers via find_open_pr_for_branch.
                    let req = crate::services::github::CreatePrRequest {
                        owner: &owner,
                        repo: &repo,
                        title,
                        body: &body,
                        head,
                        base: &base,
                    };
                    client
                        .create_pull_request_idempotent(req)
                        .map_err(|e| e.to_string())
                },
            );
            if let Some(revision) = target_revision {
                view.context.set("evidence.revision", revision.to_string());
            }
            result
        }
    })();

    let event = match action_res {
        Ok(ev) => ev,
        Err(err) if !open_pr_policy.is_some_and(|policy| policy.requires_existing()) => CircuitEvent::EffectUncertain {
            node_id: node_id.to_string(),
            attempt: view.step(node_id).map_or(1, |s| s.attempt),
            reason: format!("External action result is unknown: {err}. Inspect GitHub before recording an outcome or deliberately retrying."),
        },
        Err(err) => CircuitEvent::GithubActionResult {
            node_id: node_id.to_string(),
            success: false,
            pr_number: None,
            pr_url: None,
            pr_head_ref: None,
            pr_title: None,
            error: Some(err),
        },
    };

    tracing::info!(
        "circuits: run {} executed GitHub {:?} on {}/{}",
        active.run.id,
        action,
        owner,
        repo
    );
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn pull_request(head_ref: &str) -> crate::services::github::PullRequest {
        serde_json::from_value(serde_json::json!({
            "number": 314,
            "html_url": "https://github.com/example/buildmesh/pull/314",
            "title": "Implementation",
            "head": { "ref": head_ref }
        }))
        .unwrap()
    }

    #[test]
    fn open_pr_recheck_queries_only_the_saved_repository_and_branch() {
        let target = OpenPrEffectTarget {
            owner: "example".into(),
            repo: "buildmesh".into(),
            head: "feature/circuit".into(),
        };
        let seen = RefCell::new(None);
        let found = recheck_open_pr_target(&target, |owner, repo, head| {
            *seen.borrow_mut() = Some((owner.to_string(), repo.to_string(), head.to_string()));
            Ok(Some(pull_request(head)))
        })
        .unwrap();
        assert_eq!(
            *seen.borrow(),
            Some((
                "example".into(),
                "buildmesh".into(),
                "feature/circuit".into()
            ))
        );
        assert_eq!(found.head_ref, "feature/circuit");
    }

    #[test]
    fn open_pr_recheck_keeps_missing_and_mismatched_results_unverified() {
        let target = OpenPrEffectTarget {
            owner: "example".into(),
            repo: "buildmesh".into(),
            head: "feature/circuit".into(),
        };
        assert!(recheck_open_pr_target(&target, |_, _, _| Ok(None))
            .unwrap_err()
            .contains("did not create one"));
        assert!(recheck_open_pr_target(&target, |_, _, _| Ok(Some(pull_request("other"))))
            .unwrap_err()
            .contains("remains unverified"));
        let incomplete = OpenPrEffectTarget {
            head: String::new(),
            ..target
        };
        assert!(recheck_open_pr_target(&incomplete, |_, _, _| {
            panic!("incomplete identity must not reach GitHub")
        })
        .unwrap_err()
        .contains("no GitHub request was made"));
    }
}
