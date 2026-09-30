//! Issue dependency filtering for Circuit triggers.
use std::collections::HashSet;
use crate::services::github::{Issue, parse_blocked_by};

pub(crate) fn unresolved_blockers(
    issue: &Issue,
    open_issue_numbers: &HashSet<i64>,
    known_issue_numbers: &[i64],
) -> Option<Vec<i64>> {
    let blockers = parse_blocked_by(&issue.body);
    let unresolved: Vec<i64> = blockers
        .into_iter()
        .filter(|b| open_issue_numbers.contains(b) || known_issue_numbers.contains(b))
        .collect();
    if unresolved.is_empty() {
        None
    } else {
        Some(unresolved)
    }
}
