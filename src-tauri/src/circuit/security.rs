//! Collaborator trust policy for issue-triggered Circuits.

use crate::models::SessionStatus;
use crate::services::github::{GitHubClient, GitHubError, Issue, PullRequest};

// Re-export so external callers (and the integration test) can name the gate's
// input type through the public `circuit` path even though `services` is a
// private module — this is also what keeps `evaluate`'s public signature
// nameable (no private-in-public lint).
pub use crate::services::github::CollaboratorPermission;

/// Which GitHub object a Circuit trigger came from. The gate decision is the
/// same for both today (collaborator push-access), but the kind is carried so a
/// later policy could treat them differently (e.g. always gate fork PRs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    Issue,
    PullRequest,
}

/// A normalised Circuit trigger: the repo coordinates plus *who* raised the
/// issue/PR. `author` is the identity whose push access the gate checks — it is
/// the GitHub login, taken from the issue/PR `user.login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitTrigger {
    pub owner: String,
    pub repo: String,
    pub number: i64,
    pub author: String,
    pub kind: TriggerKind,
}

impl CircuitTrigger {
    /// Build a trigger from a fetched [`Issue`]. `owner`/`repo` are the mesh's
    /// repo coordinates (the issue body doesn't carry them); `author` and
    /// `number` come from the issue.
    pub(crate) fn from_issue(owner: &str, repo: &str, issue: &Issue) -> Self {
        Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number: issue.number,
            author: issue.author.clone(),
            kind: TriggerKind::Issue,
        }
    }

    /// Build a trigger from a fetched [`PullRequest`]. The gate checks *who
    /// opened the PR* (`pr.author`), not the fork owner — the two usually match
    /// but the author is the trust-relevant identity.
    // The Circuit poller currently builds issue triggers. This constructor
    // keeps pull-request identity normalization covered for future trigger use.
    #[allow(dead_code)]
    pub(crate) fn from_pull_request(owner: &str, repo: &str, pr: &PullRequest) -> Self {
        Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number: pr.number,
            author: pr.author.clone(),
            kind: TriggerKind::PullRequest,
        }
    }
}

/// The collaborator gate's verdict for one trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Author has push access — Circuit may spawn and run the node
    /// automatically (it follows the normal spawn lifecycle).
    AutoRun,
    /// Author lacks push access (external / first-time contributor) or is
    /// unknown — the spawned node must be parked in [`SessionStatus::Suspended`]
    /// until a human approves the run.
    RequireApproval,
}

impl GateDecision {
    /// `true` when a human must approve before the node runs.
    pub fn requires_approval(self) -> bool {
        matches!(self, GateDecision::RequireApproval)
    }

    /// The lifecycle status a Circuit-spawned node should start in under this
    /// decision. `RequireApproval` → `Suspended` (the existing parked state the
    /// desktop UI will surface an "Approve Sandbox Run" action on). `AutoRun` →
    /// `None`: the node takes the normal spawn path (`Pending` → `Running`), so
    /// the gate imposes no status override.
    pub fn initial_status(self) -> Option<SessionStatus> {
        match self {
            GateDecision::AutoRun => None,
            GateDecision::RequireApproval => Some(SessionStatus::Suspended),
        }
    }
}

/// Decide whether a trigger may auto-run, given the author's already-fetched
/// repo permission. Pure and total — no network, no DB.
///
/// An author with no login (an empty `user.login` from a partial API payload)
/// is never auto-run: an unknown identity can't be a verified collaborator, so
/// it gates regardless of the fetched permission (which for an empty username
/// would already be `None`, but the explicit guard makes the intent loud).
pub fn evaluate(trigger: &CircuitTrigger, author_permission: CollaboratorPermission) -> GateDecision {
    if trigger.author.trim().is_empty() {
        return GateDecision::RequireApproval;
    }
    if author_permission.has_push_access() {
        GateDecision::AutoRun
    } else {
        GateDecision::RequireApproval
    }
}

/// Fetch a Circuit trigger author's repository permission and apply the collaborator gate.
/// Circuit trigger ingestion owns this network seam; `evaluate` owns the pure policy.
pub(crate) fn gate_trigger(
    client: &GitHubClient,
    trigger: &CircuitTrigger,
) -> Result<GateDecision, GitHubError> {
    // Short-circuit an unknown author before the fetch: an empty login would
    // build a malformed `collaborators//permission` URL and waste a rate-limited
    // round-trip, and `evaluate` would gate it anyway.
    if trigger.author.trim().is_empty() {
        return Ok(GateDecision::RequireApproval);
    }
    let permission =
        client.collaborator_permission(&trigger.owner, &trigger.repo, &trigger.author)?;
    Ok(evaluate(trigger, permission))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(author: &str) -> CircuitTrigger {
        CircuitTrigger {
            owner: "alondero".to_string(),
            repo: "buildmesh".to_string(),
            number: 7,
            author: author.to_string(),
            kind: TriggerKind::PullRequest,
        }
    }

    #[test]
    fn collaborator_with_push_access_auto_runs() {
        let t = trigger("maintainer");
        assert_eq!(evaluate(&t, CollaboratorPermission::Admin), GateDecision::AutoRun);
        assert_eq!(evaluate(&t, CollaboratorPermission::Write), GateDecision::AutoRun);
    }

    #[test]
    fn non_pushing_collaborator_requires_approval() {
        // read/triage access can't push — an external-ish contributor must be
        // gated even though they appear in the collaborator list.
        let t = trigger("triager");
        assert_eq!(
            evaluate(&t, CollaboratorPermission::Read),
            GateDecision::RequireApproval
        );
    }

    #[test]
    fn non_collaborator_requires_approval() {
        let t = trigger("random-internet-person");
        assert_eq!(
            evaluate(&t, CollaboratorPermission::None),
            GateDecision::RequireApproval
        );
    }

    #[test]
    fn unknown_author_is_never_auto_run() {
        // Defensive: even if a buggy fetch reported push access for a blank
        // author, an unknown identity must still gate.
        let t = trigger("   ");
        assert_eq!(
            evaluate(&t, CollaboratorPermission::Admin),
            GateDecision::RequireApproval
        );
    }

    #[test]
    fn require_approval_maps_to_suspended_status() {
        // AC: external/first-time contributor triggers enter the Suspended state.
        assert_eq!(
            GateDecision::RequireApproval.initial_status(),
            Some(SessionStatus::Suspended)
        );
        assert!(GateDecision::RequireApproval.requires_approval());
    }

    #[test]
    fn auto_run_imposes_no_status_override() {
        assert_eq!(GateDecision::AutoRun.initial_status(), None);
        assert!(!GateDecision::AutoRun.requires_approval());
    }

    #[test]
    fn trigger_from_issue_carries_author_and_number() {
        // The gate checks the *issue author's* push access — pin that the
        // trigger lifts `author`/`number` from the fetched issue.
        let json = r#"{ "number": 358, "title": "x", "user": {"login": "octocat"} }"#;
        let issue: Issue = serde_json::from_str(json).expect("issue parses");
        let t = CircuitTrigger::from_issue("alondero", "buildmesh", &issue);
        assert_eq!(t.author, "octocat");
        assert_eq!(t.number, 358);
        assert_eq!(t.kind, TriggerKind::Issue);
    }

    #[test]
    fn trigger_from_pull_request_carries_author() {
        let json = r#"{
            "number": 412,
            "html_url": "https://github.com/x/y/pull/412",
            "title": "x",
            "user": {"login": "contributor-jane"},
            "head": {"ref": "feat/x"}
        }"#;
        let pr: PullRequest = serde_json::from_str(json).expect("pr parses");
        let t = CircuitTrigger::from_pull_request("alondero", "buildmesh", &pr);
        assert_eq!(t.author, "contributor-jane");
        assert_eq!(t.kind, TriggerKind::PullRequest);
    }

    /// End-to-end over the pure path: a non-collaborator PR trigger, built from
    /// a deserialised GitHub payload, gates to a Suspended node.
    #[test]
    fn external_pr_trigger_gates_to_suspended() {
        let json = r#"{
            "number": 99,
            "html_url": "https://github.com/x/y/pull/99",
            "title": "Drive-by",
            "user": {"login": "drive-by-dan"},
            "head": {"ref": "patch-1"}
        }"#;
        let pr: PullRequest = serde_json::from_str(json).expect("pr parses");
        let t = CircuitTrigger::from_pull_request("alondero", "buildmesh", &pr);
        // drive-by-dan is not a collaborator → GitHub reports `none`.
        let decision = evaluate(&t, CollaboratorPermission::None);
        assert_eq!(decision, GateDecision::RequireApproval);
        assert_eq!(decision.initial_status(), Some(SessionStatus::Suspended));
    }
}
