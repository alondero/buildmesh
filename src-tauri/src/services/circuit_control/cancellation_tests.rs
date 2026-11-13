//! The cancellation retirement helper: each agent is attempted, and a
//! partial failure is reported without hiding the ones that worked.

use super::retire_cancelled_agents_with;

#[test]
fn cancellation_retirement_attempts_every_agent_and_reports_partial_failure() {
    let mut attempted = Vec::new();
    let error = retire_cancelled_agents_with(vec![11, 12, 13], |agent_id| {
        attempted.push(agent_id);
        if agent_id == 12 {
            Err("worktree busy".to_string())
        } else {
            Ok(())
        }
    })
    .unwrap_err();

    assert_eq!(attempted, vec![11, 12, 13]);
    assert!(error.contains("agent node 12: worktree busy"));
}
