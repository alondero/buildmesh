//! Explicit report decisions, consumed only after the normal evidence preflight.

use super::turn_classify::{awaits_review_turn, is_reviewer_verdict_gate};
use super::*;
use crate::circuit::evaluator::Classification;

const REVIEW: &str = "BUILDMESH_REVIEW_V1: ";
const HANDOFF: &str = "BUILDMESH_HANDOFF_V1: ";
const REVIEW_PROMPT: &str = "Circuit result contract: finish your final review report with exactly one plain-text line: BUILDMESH_REVIEW_V1: APPROVE or BUILDMESH_REVIEW_V1: REQUEST_CHANGES or BUILDMESH_REVIEW_V1: BLOCKED (choose one). APPROVE means no actionable findings remain; REQUEST_CHANGES means actionable findings remain; BLOCKED means you cannot finish the review without help. Put findings, reviewed revision, and verification before that line. Do not quote or repeat the result line in examples, code blocks, or progress updates. Emit it only after your review and all delegated/background work have finished, or you are blocked on a person. Review completion is not approval.";
const HANDOFF_PROMPT: &str = "Circuit result contract: finish your final fixes report with exactly one plain-text line: BUILDMESH_HANDOFF_V1: READY or BUILDMESH_HANDOFF_V1: BLOCKED (choose one). READY means this fix round and all delegated/background work have finished and are ready for independent review; it does not mean approved. BLOCKED means you need a person to proceed. Explain changes, verification, disagreements or blockers before that line. Do not quote or repeat the result line in examples, code blocks, or progress updates.";
const PUBLISH_PROMPT: &str = "Circuit result contract: finish your final publication report with exactly one plain-text line: BUILDMESH_HANDOFF_V1: READY or BUILDMESH_HANDOFF_V1: BLOCKED (choose one). READY means your work is committed and pushed and has an open pull request, ready for independent review; it does not mean approved. BLOCKED means you need a person to proceed. Give the pull request URL or the blocker before that line. Do not quote or repeat the result line in examples, code blocks, or progress updates.";
const ISSUE_HANDOFF_PROMPT: &str = "Circuit result contract: finish your final report for this assigned phase (implementation, wrap-up/publication, or review fixes) with exactly one plain-text line: BUILDMESH_HANDOFF_V1: READY or BUILDMESH_HANDOFF_V1: BLOCKED (choose one). READY means all instructions for this phase and all delegated/background work have finished; the next circuit gate may proceed. It does not mean independently reviewed or approved. BLOCKED means you need a person to proceed. Explain changes, verification and any blockers before that line. Do not quote or repeat the result line in examples, code blocks, or progress updates. Emit it only in the final report.";

fn is_review(view: &RunView, node_id: &str) -> bool {
    is_reviewer_verdict_gate(view, node_id) || spawn::is_review_spawn_step(view, node_id)
}

fn is_issue_handoff(view: &RunView, node_id: &str) -> bool {
    if !view.graph.is_issue_driven_autopilot_review() {
        return false;
    }
    match view.graph.node(node_id).map(|node| &node.kind) {
        Some(CircuitNodeKind::SpawnAgentNode { .. }) => node_id == "implementer",
        Some(CircuitNodeKind::InjectPty { target_node_id, .. }) => {
            matches!(
                node_id,
                "implementation_prompt" | "finish" | "wrapup_correction" | "follow_feedback"
            ) && target_node_id.as_deref() == Some("implementer")
        }
        Some(CircuitNodeKind::LlmTurnClassifier { target_node_id }) => {
            matches!(
                node_id,
                "implementation_classifier" | "finish_classifier" | "feedback_classifier"
            ) && target_node_id.as_deref() == Some("implementer")
        }
        _ => false,
    }
}

/// A later review round prompts the reviewer that is already open; its next
/// report is judged by the same verdict gate as its first.
fn reprompts_reviewer(view: &RunView, node_id: &str) -> bool {
    matches!(
        view.graph.node(node_id).map(|node| &node.kind),
        Some(CircuitNodeKind::InjectPty { target_node_id: Some(target), .. })
            if spawn::is_review_spawn_step(view, target)
    )
}

/// The result contract a dispatch to this node carries, if any.
fn suffix(view: &RunView, node_id: &str, prompt: &str) -> Option<&'static str> {
    if spawn::is_review_spawn_step(view, node_id) || reprompts_reviewer(view, node_id) {
        Some(REVIEW_PROMPT)
    } else if node_id == "feedback" && awaits_review_turn(view, "await_fixes") {
        Some(HANDOFF_PROMPT)
    } else if node_id == "publish" && awaits_review_turn(view, "await_publish") {
        Some(PUBLISH_PROMPT)
    } else if is_issue_handoff(view, node_id) && !prompt.trim().is_empty() {
        Some(ISSUE_HANDOFF_PROMPT)
    } else {
        None
    }
}

/// Append at dispatch so saved/custom prompts get the same transport contract
/// without rewriting their review instructions or pinned graph.
pub(super) fn prompt(view: &RunView, node_id: &str, prompt: &str) -> String {
    suffix(view, node_id, prompt).map_or_else(
        || prompt.to_owned(),
        |suffix| format!("{prompt}\n\n{suffix}"),
    )
}

/// Whether a dispatch of `prompt` to this node carries a result contract, and so
/// expects its final report in a result file as well as the transcript.
pub(super) fn requests_result(view: &RunView, node_id: &str, prompt: &str) -> bool {
    suffix(view, node_id, prompt).is_some()
}

// Missing contracts retain legacy interpretation. A present but malformed
// contract must never fall through to a model that might guess approval.
fn parse<'a>(output: &'a str, prefix: &str) -> Result<Option<&'a str>, ()> {
    let family = prefix.strip_suffix("V1: ").unwrap_or(prefix);
    if !output.contains(family) {
        return Ok(None);
    }
    if output.matches(family).count() != 1 {
        return Err(());
    }
    let (body, last) = output.trim_end().rsplit_once('\n').ok_or(())?;
    if body.trim().is_empty() {
        return Err(());
    }
    let mut fence: Option<(char, usize)> = None;
    for line in body.lines() {
        let line = line.trim_start();
        let Some(first @ ('`' | '~')) = line.chars().next() else {
            continue;
        };
        let width = line.chars().take_while(|c| *c == first).count();
        if width < 3 {
            continue;
        }
        match fence {
            None => fence = Some((first, width)),
            Some((delimiter, minimum))
                if first == delimiter && width >= minimum && line[width..].trim().is_empty() =>
            {
                fence = None
            }
            _ => {}
        }
    }
    if fence.is_some() {
        return Err(());
    }
    last.strip_prefix(prefix)
        .map(|value| Some(value.trim_end()))
        .ok_or(())
}

fn result(view: &RunView, node_id: &str, output: &str) -> Result<Option<Classification>, ()> {
    if is_review(view, node_id) {
        match parse(output, REVIEW)? {
            None => Ok(None),
            Some("APPROVE") => Ok(Some(Classification::Completed)),
            Some("REQUEST_CHANGES") => Ok(Some(Classification::Working)),
            Some("BLOCKED") => Ok(Some(Classification::Blocked)),
            _ => Err(()),
        }
    } else if awaits_review_turn(view, node_id) || is_issue_handoff(view, node_id) {
        match parse(output, HANDOFF)? {
            None => Ok(None),
            Some("READY") => Ok(Some(Classification::Completed)),
            Some("BLOCKED") => Ok(Some(Classification::Blocked)),
            _ => Err(()),
        }
    } else {
        Ok(None)
    }
}

pub(super) fn declares_completion(view: &RunView, node_id: &str, output: &str) -> bool {
    matches!(result(view, node_id, output), Ok(Some(_)))
}

pub(super) fn interpretation(
    view: &RunView,
    node_id: &str,
    output: &str,
) -> Option<Classification> {
    result(view, node_id, output).unwrap_or(Some(Classification::Blocked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_handoffs_dispatch_and_interpret_without_inference() {
        let mut view = RunView {
            run_id: 280,
            state: RunState::Running,
            graph: CircuitGraph::issue_driven_autopilot_review("ready-for-agent"),
            context: CircuitContext::new(),
            steps: vec![],
        };
        for dispatch in [
            "implementer",
            "finish",
            "wrapup_correction",
            "follow_feedback",
        ] {
            let dispatched = prompt(&view, dispatch, "Custom phase instructions");
            assert!(dispatched.starts_with("Custom phase instructions\n\n"));
            assert!(
                dispatched.contains("BUILDMESH_HANDOFF_V1: READY"),
                "{dispatch}"
            );
        }
        for gate in [
            "implementer",
            "implementation_classifier",
            "finish_classifier",
            "feedback_classifier",
        ] {
            for (value, expected) in [
                ("READY", Classification::Completed),
                ("BLOCKED", Classification::Blocked),
            ] {
                let report =
                    format!("Phase details and verification.\nBUILDMESH_HANDOFF_V1: {value}");
                assert!(declares_completion(&view, gate, &report), "{gate}");
                assert_eq!(interpretation(&view, gate, &report), Some(expected));
            }
            for report in [
                "Details.\nBUILDMESH_HANDOFF_V2: READY",
                "Details.\nBUILDMESH_HANDOFF_V1: MAYBE",
                "Details.\nBUILDMESH_HANDOFF_V1: READY\nStill working",
            ] {
                assert!(!declares_completion(&view, gate, report));
                assert_eq!(
                    interpretation(&view, gate, report),
                    Some(Classification::Blocked)
                );
            }
        }
        assert_eq!(
            interpretation(
                &view,
                "review_classifier",
                "Fixes.\nBUILDMESH_HANDOFF_V1: READY"
            ),
            None
        );
        assert_eq!(
            interpretation(&view, "implementation_classifier", "All work is done."),
            None
        );
        assert_eq!(
            prompt(&view, "implementer", ""),
            "",
            "allocation-only spawns must not acquire a turn"
        );
        view.graph.blueprint = None;
        assert_eq!(prompt(&view, "implementer", "Custom task"), "Custom task");
        assert_eq!(
            interpretation(
                &view,
                "implementation_classifier",
                "Done.\nBUILDMESH_HANDOFF_V1: READY"
            ),
            None
        );
    }

    #[test]
    fn saved_issue_handoff_roles_preserve_allocation_and_target_scope() {
        let mut view = RunView {
            run_id: 276,
            state: RunState::Running,
            graph: CircuitGraph::from_json(include_str!(
                "../../../tests/fixtures/legacy-issue-review-circuit.json"
            ))
            .unwrap(),
            context: CircuitContext::new(),
            steps: vec![],
        };
        assert!(prompt(&view, "implementer", "Saved issue instructions").contains(HANDOFF));
        view.graph.nodes.push(crate::circuit::model::CircuitNode {
            id: "implementation_prompt".into(),
            kind: CircuitNodeKind::InjectPty {
                prompt: "Saved issue instructions".into(),
                target_node_id: Some("implementer".into()),
            },
        });
        assert_eq!(prompt(&view, "implementer", ""), "");
        assert!(
            prompt(&view, "implementation_prompt", "Saved issue instructions").contains(HANDOFF)
        );
        let gate = view
            .graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "implementation_classifier")
            .unwrap();
        gate.kind = CircuitNodeKind::LlmTurnClassifier {
            target_node_id: Some("reviewer".into()),
        };
        assert_eq!(
            interpretation(
                &view,
                "implementation_classifier",
                "Done.\nBUILDMESH_HANDOFF_V1: READY"
            ),
            None
        );
    }

    #[test]
    fn result_requires_one_unquoted_terminal_line_and_report_body() {
        assert_eq!(
            parse("Details.\nBUILDMESH_REVIEW_V1: APPROVE\n", REVIEW),
            Ok(Some("APPROVE"))
        );
        assert_eq!(
            parse(
                "Details.\n```rust\ncode\n```\nBUILDMESH_REVIEW_V1: APPROVE",
                REVIEW
            ),
            Ok(Some("APPROVE"))
        );
        assert_eq!(parse("Approved, no findings", REVIEW), Ok(None));
        for report in [
            "BUILDMESH_REVIEW_V1: APPROVE",
            "Details.\n> BUILDMESH_REVIEW_V1: APPROVE",
            "Details.\n```\nBUILDMESH_REVIEW_V1: APPROVE",
            "Details.\n~~~~\n```\nBUILDMESH_REVIEW_V1: APPROVE",
            "Details.\nBUILDMESH_REVIEW_V1: APPROVE\nStill working",
            "BUILDMESH_REVIEW_V1: BLOCKED\nBUILDMESH_REVIEW_V1: APPROVE",
            "Example BUILDMESH_REVIEW_V1: APPROVE\nDone",
            "Details.\nBUILDMESH_REVIEW_V2: APPROVE",
        ] {
            assert!(parse(report, REVIEW).is_err(), "{report}");
        }
    }

    #[test]
    fn publication_flow_prompts_carry_the_contract_their_gate_reads() {
        for (graph, verdict) in [
            (CircuitGraph::agent_review(None, None, 3), "verdict"),
            (
                CircuitGraph::issue_driven_autopilot_review("ready-for-agent"),
                "review_classifier",
            ),
        ] {
            let mut view = RunView {
                run_id: 1,
                state: RunState::Running,
                graph,
                context: CircuitContext::new(),
                steps: vec![],
            };
            view.context.set("source.review_preset", "1");
            let rereview = prompt(&view, "re_review", "Review again");
            assert!(rereview.starts_with("Review again\n\n"), "{verdict}");
            assert!(
                rereview.contains("BUILDMESH_REVIEW_V1: REQUEST_CHANGES"),
                "{verdict}"
            );
            assert_eq!(
                interpretation(&view, verdict, "Findings.\nBUILDMESH_REVIEW_V1: APPROVE"),
                Some(Classification::Completed)
            );
            // Nothing waits on the merge turn, so it carries no contract.
            assert_eq!(
                prompt(&view, "merge", "Squash and merge"),
                "Squash and merge",
                "{verdict}"
            );
        }

        let mut local = RunView {
            run_id: 1,
            state: RunState::Running,
            graph: CircuitGraph::agent_review(None, None, 3),
            context: CircuitContext::new(),
            steps: vec![],
        };
        local.context.set("source.review_preset", "1");
        let publish = prompt(&local, "publish", "Publish your work");
        assert!(publish.starts_with("Publish your work\n\n"));
        assert!(
            publish.contains("BUILDMESH_HANDOFF_V1: READY") && publish.contains("pull request")
        );
        assert_eq!(
            interpretation(
                &local,
                "await_publish",
                "PR #7 opened.\nBUILDMESH_HANDOFF_V1: READY"
            ),
            Some(Classification::Completed)
        );
        assert_eq!(
            interpretation(
                &local,
                "await_publish",
                "No GitHub access.\nBUILDMESH_HANDOFF_V1: BLOCKED"
            ),
            Some(Classification::Blocked)
        );
    }

    #[test]
    fn dispatch_contract_and_decisions_follow_the_gate_role() {
        let mut view = RunView {
            run_id: 1,
            state: RunState::Running,
            graph: CircuitGraph::agent_review(None, None, 3),
            context: CircuitContext::new(),
            steps: vec![],
        };
        view.context.set("source.review_preset", "1");
        let reviewer = prompt(&view, "reviewer", "Custom review instructions");
        assert!(reviewer.starts_with("Custom review instructions\n\n"));
        assert!(reviewer.contains("BUILDMESH_REVIEW_V1: REQUEST_CHANGES"));
        let feedback = prompt(&view, "feedback", "Fix the findings");
        assert!(feedback.starts_with("Fix the findings\n\n"));
        assert!(feedback.contains("BUILDMESH_HANDOFF_V1: READY"));
        assert_eq!(prompt(&view, "approved", "Unchanged"), "Unchanged");
        assert_eq!(
            interpretation(&view, "await_fixes", "Fixed.\nBUILDMESH_HANDOFF_V1: READY"),
            Some(Classification::Completed)
        );
        assert_eq!(
            interpretation(
                &view,
                "await_fixes",
                "Need help.\nBUILDMESH_HANDOFF_V1: BLOCKED"
            ),
            Some(Classification::Blocked)
        );
        assert_eq!(
            interpretation(&view, "verdict", "Fixed.\nBUILDMESH_HANDOFF_V1: READY"),
            None
        );
        for output in [
            "Details.\nBUILDMESH_REVIEW_V1: MAYBE",
            "Details.\nBUILDMESH_REVIEW_V2: APPROVE",
        ] {
            assert_eq!(
                interpretation(&view, "verdict", output),
                Some(Classification::Blocked)
            );
            assert!(!declares_completion(&view, "verdict", output));
        }
        assert_eq!(
            interpretation(&view, "verdict", "Approved. No findings."),
            None
        );
        view.context.set("source.review_preset", "0");
        assert_eq!(
            interpretation(&view, "await_fixes", "Fixed.\nBUILDMESH_HANDOFF_V1: READY"),
            None
        );
        assert_eq!(prompt(&view, "feedback", "Fix"), "Fix");
    }

    #[test]
    fn requests_result_follows_the_contract_the_prompt_carries() {
        let view = RunView {
            run_id: 280,
            state: RunState::Running,
            graph: CircuitGraph::issue_driven_autopilot_review("ready-for-agent"),
            context: CircuitContext::new(),
            steps: vec![],
        };
        assert!(requests_result(
            &view,
            "implementer",
            "Custom phase instructions"
        ));
        // An allocation-only spawn carries no contract, so it expects no result file.
        assert!(!requests_result(&view, "implementer", ""));
        assert!(!requests_result(&view, "approved", "Unchanged"));

        let mut review = RunView {
            run_id: 1,
            state: RunState::Running,
            graph: CircuitGraph::agent_review(None, None, 3),
            context: CircuitContext::new(),
            steps: vec![],
        };
        review.context.set("source.review_preset", "1");
        assert!(requests_result(
            &review,
            "reviewer",
            "Custom review instructions"
        ));
        assert!(!requests_result(&review, "approved", "Unchanged"));
    }
}
