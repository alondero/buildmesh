//! Explicit report decisions, consumed only after the normal evidence preflight.

use super::*;
use crate::autopilot::evaluator::Classification;

const REVIEW: &str = "BUILDMESH_REVIEW_V1: ";
const HANDOFF: &str = "BUILDMESH_HANDOFF_V1: ";
const REVIEW_PROMPT: &str = "Circuit result contract: finish your final review report with exactly one plain-text line: BUILDMESH_REVIEW_V1: APPROVE or BUILDMESH_REVIEW_V1: REQUEST_CHANGES or BUILDMESH_REVIEW_V1: BLOCKED (choose one). APPROVE means no actionable findings remain; REQUEST_CHANGES means actionable findings remain; BLOCKED means you cannot finish the review without help. Put findings, reviewed revision, and verification before that line. Do not quote or repeat the result line in examples, code blocks, or progress updates. Emit it only after your review and all delegated/background work have finished, or you are blocked on a person. Review completion is not approval.";
const HANDOFF_PROMPT: &str = "Circuit result contract: finish your final fixes report with exactly one plain-text line: BUILDMESH_HANDOFF_V1: READY or BUILDMESH_HANDOFF_V1: BLOCKED (choose one). READY means this fix round and all delegated/background work have finished and are ready for independent review; it does not mean approved. BLOCKED means you need a person to proceed. Explain changes, verification, disagreements or blockers before that line. Do not quote or repeat the result line in examples, code blocks, or progress updates.";

fn is_review(view: &RunView, node_id: &str) -> bool {
    is_reviewer_verdict_gate(view, node_id) || spawn::is_review_spawn_step(view, node_id)
}

/// Append at dispatch so saved/custom prompts get the same transport contract
/// without rewriting their review instructions or pinned graph.
pub(super) fn prompt(view: &RunView, node_id: &str, prompt: &str) -> String {
    let suffix = if spawn::is_review_spawn_step(view, node_id) {
        Some(REVIEW_PROMPT)
    } else if node_id == "feedback" && awaits_review_turn(view, "await_fixes") {
        Some(HANDOFF_PROMPT)
    } else {
        None
    };
    suffix.map_or_else(
        || prompt.to_owned(),
        |suffix| format!("{prompt}\n\n{suffix}"),
    )
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
    } else if awaits_review_turn(view, node_id) {
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
}
