//! GitHub effects for the circuit worker (issue #1660).
//! A new GitHub action kind is added here, not in the observe-step-commit loop.

use crate::circuit::model::CircuitNodeKind;
use crate::circuit::stepper::{CircuitEvent, RunView};
use crate::db;


/// Determine the target (issue vs PR number) for a GitHub action.
/// If the action is CloseIssue, it explicitly requires an issue trigger.
/// If this node has an upstream OpenPr node in its lineage, it targets that PR (pr.number).
/// Otherwise, it falls back to issue.number if present, then pr.number.
pub(super) fn determine_github_target(
    view: &RunView,
    node_id: &str,
    action: crate::circuit::model::GithubActionKind,
) -> Result<(&'static str, i64), String> {
    use crate::circuit::model::GithubActionKind;
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

/// Decide whether the run's pull request was merged, from an injected GitHub
/// read so the decision is testable without a network.
///
/// Every answer is a `GithubActionResult`: merged is `success`, anything else
/// is a failure the blueprint routes (the work is left open), never a guess.
/// "Could not read the PR" and "the PR is not merged" are both failures, but the
/// reason says which, so the person reading it knows whether to look at GitHub.
pub(super) fn confirm_pr_merged(
    view: &RunView,
    node_id: &str,
    lookup: impl FnOnce(i64) -> Result<crate::services::github::PullRequestMergeState, String>,
) -> CircuitEvent {
    let failure = |error: String| CircuitEvent::GithubActionResult {
        node_id: node_id.to_string(),
        success: false,
        pr_number: None,
        pr_url: None,
        pr_head_ref: None,
        pr_title: None,
        error: Some(error),
    };
    let Some(number) = view
        .context
        .get("pr.number")
        .and_then(|n| n.parse::<i64>().ok())
    else {
        return failure("no pull request number is recorded for this run".into());
    };
    match lookup(number) {
        Err(error) => failure(format!(
            "GitHub could not confirm the merge of PR #{number}: {error}"
        )),
        Ok(state) if state.merged => CircuitEvent::GithubActionResult {
            node_id: node_id.to_string(),
            success: true,
            pr_number: Some(number),
            pr_url: None,
            pr_head_ref: None,
            pr_title: None,
            error: None,
        },
        Ok(state) if state.state == "closed" => {
            failure(format!("PR #{number} was closed without being merged"))
        }
        Ok(_) => failure(format!("PR #{number} is still open and has not been merged")),
    }
}

/// Reconcile the implementation branch with GitHub before emitting the durable
/// result. Inject the external observations so replay and lookup failures can
/// be exercised without a process-wide database or GitHub writes.
#[cfg(test)]
pub(super) fn ensure_open_pr(
    view: &RunView,
    node_id: &str,
    policy: Option<crate::circuit::model::OpenPrPolicy>,
    observe: impl FnOnce(i64) -> Result<crate::circuit::verification::WrapupState, String>,
    find: impl FnOnce(&str) -> Result<Option<crate::services::github::PullRequest>, String>,
    create: impl FnOnce(&str, &str) -> Result<crate::services::github::PullRequest, String>,
) -> Result<CircuitEvent, String> {
    ensure_open_pr_with_target(view, node_id, policy, observe, |_| Ok(()), find, create)
}

pub(super) fn ensure_open_pr_with_target(
    view: &RunView,
    node_id: &str,
    policy: Option<crate::circuit::model::OpenPrPolicy>,
    observe: impl FnOnce(i64) -> Result<crate::circuit::verification::WrapupState, String>,
    record_target: impl FnOnce(&str) -> Result<(), String>,
    find: impl FnOnce(&str) -> Result<Option<crate::services::github::PullRequest>, String>,
    create: impl FnOnce(&str, &str) -> Result<crate::services::github::PullRequest, String>,
) -> Result<CircuitEvent, String> {
    let agent_node_id = view
        .resolve_open_pr_agent(node_id)
        .ok_or_else(|| "OpenPr requires a spawned agent earlier in this run".to_string())?;
    let wrapup = observe(agent_node_id)?;
    let reasons = crate::circuit::verification::wrapup_reasons(&wrapup);
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

pub(super) fn reconcile_open_pr_effect_for_worker(
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    node_id: &str,
) -> CircuitEvent {
    use crate::services::github::GitHubClient;

    match GitHubClient::new() {
        Ok(client) => {
            reconcile_open_pr_effect_for_worker_with_client(active, view, node_id, &client)
        }
        Err(error) => {
            reconcile_open_pr_for_worker(active, view, node_id, |_, _, _| Err(error.to_string()))
        }
    }
}

/// Same worker handoff with an explicit client, so the deterministic coverage
/// can drive the production lookup-to-outcome mapping against a controllable
/// endpoint instead of the live API. Production always passes
/// `GitHubClient::new()`.
pub(super) fn reconcile_open_pr_effect_for_worker_with_client(
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    node_id: &str,
    client: &crate::services::github::GitHubClient,
) -> CircuitEvent {
    reconcile_open_pr_for_worker(active, view, node_id, |owner, repo, head| {
        client
            .find_open_pr_for_branch(owner, repo, head)
            .map_err(|error| error.to_string())
    })
}

/// Resolve the saved OpenPr target through the read-only lookup and produce
/// the worker outcome. The injected lookup is the same boundary used by the
/// production GitHub client, so tests can exercise the complete handoff
/// without making a live request or creating a pull request.
fn reconcile_open_pr_effect_with_lookup(
    active: &db::ActiveCircuitRun,
    view: &RunView,
    node_id: &str,
    find: impl FnOnce(&str, &str, &str) -> Result<Option<crate::services::github::PullRequest>, String>,
) -> Result<CircuitEvent, String> {
    let attempt = view.step(node_id).map_or(1, |step| step.attempt);
    let detail = db::circuit::evidence::latest_effect_target(active.run.id, node_id, attempt)?
        .ok_or_else(|| {
            "No durable OpenPr target is recorded for this attempt; no GitHub request was made."
                .to_string()
        })?;
    let target: OpenPrEffectTarget = serde_json::from_str(&detail).map_err(|_| {
        "The saved OpenPr target cannot be read; no GitHub request was made.".to_string()
    })?;
    let pull_request = recheck_open_pr_target(&target, find)?;
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

/// Worker handoff shared by the production GitHub call and its deterministic
/// integration coverage. Only a matching read-only result marks the existing
/// attempt reconciled; missing, mismatched, or failed lookups stay uncertain.
pub(super) fn reconcile_open_pr_for_worker(
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    node_id: &str,
    find: impl FnOnce(&str, &str, &str) -> Result<Option<crate::services::github::PullRequest>, String>,
) -> CircuitEvent {
    let attempt = view.step(node_id).map_or(1, |step| step.attempt);
    view.context
        .set(&format!("node.{node_id}.recheck_only"), "0");
    let result = reconcile_open_pr_effect_with_lookup(active, view, node_id, find);
    if let Ok(CircuitEvent::GithubActionResult {
        success: true,
        pr_number: Some(number),
        pr_url: Some(url),
        pr_head_ref: Some(head),
        ..
    }) = &result
    {
        view.context.set(
            &format!("node.{node_id}.effect_reconciled_attempt"),
            attempt.to_string(),
        );
        view.context.set(
                &format!("node.{node_id}.effect_reconciled_detail"),
                format!("Read-only GitHub lookup found open pull request #{number} ({url}) on branch {head}."),
            );
    }
    result.unwrap_or_else(|reason| CircuitEvent::EffectUncertain {
        node_id: node_id.to_string(),
        attempt,
        reason: format!("Read-only pull-request recheck could not establish the result: {reason}"),
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
    action: crate::circuit::model::GithubActionKind,
    label: Option<&str>,
    comment: Option<&str>,
) -> Result<CircuitEvent, String> {
    use crate::circuit::model::GithubActionKind;
    use crate::services::github::GitHubClient;

    if action == GithubActionKind::OpenPr
        && crate::circuit::stepper::resolve_upstream_spawn_agent(
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
        GithubActionKind::ConfirmPrMerged => Ok(confirm_pr_merged(view, node_id, |number| {
            client
                .pull_request_merge_state(&owner, &repo, number)
                .map_err(|e| e.to_string())
        })),
        GithubActionKind::OpenPr => {
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
                    Ok(crate::circuit::verification::observe_wrapup_git_state(
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

    fn merge_state(state: &str, merged: bool) -> crate::services::github::PullRequestMergeState {
        crate::services::github::PullRequestMergeState {
            merged,
            state: state.into(),
            merged_at: merged.then(|| "2026-10-05T20:00:00Z".into()),
        }
    }

    fn view_with_pr(pr: Option<&str>) -> RunView {
        use crate::circuit::{context::CircuitContext, model::CircuitGraph, stepper::RunState};
        let mut context = CircuitContext::new();
        if let Some(pr) = pr {
            context.set("pr.number", pr);
        }
        RunView {
            run_id: 1,
            graph: CircuitGraph::issue_driven_autopilot_review("buildmesh:run"),
            state: RunState::Running,
            context,
            steps: vec![],
        }
    }

    fn outcome(event: CircuitEvent) -> (bool, Option<i64>, Option<String>) {
        match event {
            CircuitEvent::GithubActionResult {
                success,
                pr_number,
                error,
                ..
            } => (success, pr_number, error),
            other => panic!("expected a GitHub action result, got {other:?}"),
        }
    }

    #[test]
    fn confirm_merged_succeeds_only_when_github_says_the_pr_merged() {
        let seen = RefCell::new(None);
        let event = confirm_pr_merged(&view_with_pr(Some("314")), "merge_verify", |number| {
            *seen.borrow_mut() = Some(number);
            Ok(merge_state("closed", true))
        });
        assert_eq!(*seen.borrow(), Some(314), "asks about the run's own PR");
        assert_eq!(outcome(event), (true, Some(314), None));
    }

    #[test]
    fn confirm_merged_reports_an_open_pr_as_not_merged() {
        let (success, _, error) = outcome(confirm_pr_merged(
            &view_with_pr(Some("314")),
            "merge_verify",
            |_| Ok(merge_state("open", false)),
        ));
        assert!(!success);
        assert!(error.unwrap().contains("still open"));
    }

    #[test]
    fn confirm_merged_does_not_mistake_a_closed_unmerged_pr_for_a_merge() {
        let (success, _, error) = outcome(confirm_pr_merged(
            &view_with_pr(Some("314")),
            "merge_verify",
            |_| Ok(merge_state("closed", false)),
        ));
        assert!(!success);
        assert!(error.unwrap().contains("closed without being merged"));
    }

    #[test]
    fn confirm_merged_treats_an_unreadable_pr_as_unconfirmed_never_as_merged() {
        let (success, _, error) = outcome(confirm_pr_merged(
            &view_with_pr(Some("314")),
            "merge_verify",
            |_| Err("network unreachable".into()),
        ));
        assert!(!success);
        let error = error.unwrap();
        assert!(error.contains("could not confirm") && error.contains("network unreachable"));
    }

    #[test]
    fn confirm_merged_never_calls_github_without_a_recorded_pr() {
        let (success, _, error) = outcome(confirm_pr_merged(
            &view_with_pr(None),
            "merge_verify",
            |_| panic!("no PR number means no GitHub request"),
        ));
        assert!(!success);
        assert!(error.unwrap().contains("no pull request number"));
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
