//! GitHub effects for the circuit worker (issue #1660).
//! A new GitHub action kind is added here, not in the observe-step-commit loop.

use tauri::AppHandle;

use crate::autopilot::circuit::model::CircuitNodeKind;
use crate::autopilot::circuit::stepper::{advance, CircuitEvent, RunView};
use crate::db;

use super::{execute_effects, prepare_turn_boundaries};

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
pub(super) fn ensure_open_pr(
    view: &RunView,
    node_id: &str,
    policy: Option<crate::autopilot::circuit::model::OpenPrPolicy>,
    observe: impl FnOnce(i64) -> Result<crate::autopilot::pipeline::WrapupState, String>,
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

/// Perform one `CallGithub` effect (milestone 3, issue #1208): resolve
/// the target repo from the mesh's `origin`, execute the mutation through
/// the shared [`crate::services::github::GitHubClient`] seam, and advance
/// the stepper with the result so context updates (e.g. `pr.*`) commit atomically
/// before downstream nodes cascade.
pub(super) fn call_github_effect(
    app: &AppHandle,
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    node_id: &str,
    action: crate::autopilot::circuit::model::GithubActionKind,
    label: Option<&str>,
    comment: Option<&str>,
) -> Result<(), String> {
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
            ensure_open_pr(
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
            )
        }
    })();

    let event = match action_res {
        Ok(ev) => ev,
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

    let transition = advance(view, &event);
    // GitHub actions can cascade directly into a wrap-up correction prompt.
    // Capture that prompt's pre-turn report in this same transition commit;
    // otherwise the recursive effect path would bypass the normal drive loop.
    let turn_boundary_changed = prepare_turn_boundaries(view, &transition.effects)?;
    if !transition.step_writes.is_empty()
        || transition.run_state_changed
        || transition.context_changed
        || turn_boundary_changed
    {
        let ops = transition
            .step_writes
            .iter()
            .map(|w| db::CircuitStepOp {
                node_id: w.node_id.clone(),
                status: w.status.as_db_str().to_string(),
                outcome: w.outcome.map(|o| o.map(|v| v.as_db_str().to_string())),
                error: w.error.clone(),
                agent_node_id: None,
                attempt: w.attempt,
                fresh_attempt: w.fresh_attempt,
            })
            .collect::<Vec<_>>();
        let run_state = if transition.run_state_changed {
            Some(view.state.as_db_str())
        } else {
            None
        };
        db::commit_circuit_advance(
            active.run.id,
            run_state,
            Some(&view.context.to_json()?),
            &ops,
        )
        .map_err(|e| format!("commit failed: {}", e))?;
    }

    if !transition.effects.is_empty() {
        execute_effects(app, active, view, &transition.effects)?;
    }

    tracing::info!(
        "circuits: run {} executed GitHub {:?} on {}/{}",
        active.run.id,
        action,
        owner,
        repo
    );
    Ok(())
}
