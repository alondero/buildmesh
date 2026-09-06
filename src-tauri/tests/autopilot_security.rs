//! Integration test for the Autopilot security gate and secret scrubber
//! (issue #499, ADR-0012 §5). Exercises the crate's *public* surface end-to-end
//! — the same entry points the future Autopilot trigger pipeline and the
//! coordinator log path call — rather than reaching into module internals:
//!   * `autopilot::evaluate` — the collaborator gate decision
//!   * `GateDecision::initial_status` — the Suspended mapping for external runs
//!   * `secret_scrubber::SecretScrubber::scrub` — log masking
//!
//! The live GitHub fetch (`gate_trigger`) is deliberately *not* exercised here:
//! it needs a network and credentials, and the decision logic it delegates to is
//! what these assertions pin.

use buildmesh_lib::autopilot::{
    evaluate, AutopilotTrigger, CollaboratorPermission, GateDecision, TriggerKind,
};
use buildmesh_lib::models::SessionStatus;
use buildmesh_lib::secret_scrubber::SecretScrubber;

fn trigger(author: &str, kind: TriggerKind) -> AutopilotTrigger {
    AutopilotTrigger {
        owner: "alondero".to_string(),
        repo: "buildmesh".to_string(),
        number: 499,
        author: author.to_string(),
        kind,
    }
}

// --- Gating logic ----------------------------------------------------------

#[test]
fn collaborator_pr_auto_runs() {
    // A maintainer (push access) opening a PR is auto-run — no approval gate.
    let t = trigger("maintainer-mo", TriggerKind::PullRequest);
    let decision = evaluate(&t, CollaboratorPermission::Write);
    assert_eq!(decision, GateDecision::AutoRun);
    assert!(!decision.requires_approval());
    assert_eq!(
        decision.initial_status(),
        None,
        "auto-run imposes no status override"
    );
}

#[test]
fn admin_issue_auto_runs() {
    let t = trigger("owner-olive", TriggerKind::Issue);
    assert_eq!(
        evaluate(&t, CollaboratorPermission::Admin),
        GateDecision::AutoRun
    );
}

#[test]
fn external_contributor_pr_is_suspended_for_approval() {
    // The headline acceptance criterion: a non-collaborator (GitHub reports
    // `none`) PR trigger is gated — the node enters Suspended pending a manual
    // "Approve Sandbox Run".
    let t = trigger("drive-by-dan", TriggerKind::PullRequest);
    let decision = evaluate(&t, CollaboratorPermission::None);
    assert_eq!(decision, GateDecision::RequireApproval);
    assert!(decision.requires_approval());
    assert_eq!(decision.initial_status(), Some(SessionStatus::Suspended));
}

#[test]
fn read_only_collaborator_is_gated() {
    // read/triage access can't push code, so it does not clear the gate.
    let t = trigger("reader-rae", TriggerKind::Issue);
    assert_eq!(
        evaluate(&t, CollaboratorPermission::Read).initial_status(),
        Some(SessionStatus::Suspended)
    );
}

#[test]
fn unknown_author_is_gated_even_with_push_permission() {
    // Defensive: an empty author login (partial payload) never auto-runs.
    let t = trigger("", TriggerKind::PullRequest);
    assert_eq!(
        evaluate(&t, CollaboratorPermission::Admin),
        GateDecision::RequireApproval
    );
}

// --- Secret masking --------------------------------------------------------

#[test]
fn scrubber_masks_representative_secrets() {
    let cases = [
        (
            "export DB_PASSWORD=hunter2",
            "export DB_PASSWORD=[REDACTED]",
        ),
        (
            r#"{"api_token": "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"}"#,
            r#"{"api_token": "[REDACTED]"}"#,
        ),
        (
            "Authorization: Bearer abc123def456ghi789",
            "Authorization: Bearer [REDACTED]",
        ),
        (
            "aws key AKIAIOSFODNN7EXAMPLE here",
            "aws key [REDACTED] here",
        ),
    ];
    for (raw, expected) in cases {
        assert_eq!(SecretScrubber::scrub(raw), expected, "scrubbing: {raw}");
    }
}

#[test]
fn scrubber_leaves_benign_transcript_text_intact() {
    let benign = "Ran cargo test on feat/x — 42 passed. author: octocat";
    assert_eq!(SecretScrubber::scrub(benign), benign);
}

#[test]
fn scrubber_masks_private_key_block_whole() {
    let pem = concat!(
        "key follows\n",
        "-----BEGIN OPENSSH PRIVATE KEY-----\n",
        "b3BlbnNzaC1rZXktdjEAAAAABG5vbmU=\n",
        "-----END OPENSSH PRIVATE KEY-----\n",
        "done"
    );
    let scrubbed = SecretScrubber::scrub(pem);
    assert!(scrubbed.contains("[REDACTED PRIVATE KEY]"));
    assert!(
        !scrubbed.contains("b3BlbnNzaC1rZXktdjEA"),
        "key body must not survive"
    );
}
