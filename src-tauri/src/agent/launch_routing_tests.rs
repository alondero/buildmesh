//! Tests for `agent::launch_routing`.
//!
//! The owner keeps only the sibling module declaration; the bodies live here
//! so the production file carries no test code (the repo's rust-test-layout
//! gate).

use super::*;
use crate::agent::provider::adapters::codex::CodexInstall;
use crate::services::provider_verification::VerificationAdvisory;

fn codex_install(version: &str) -> CodexInstall {
    CodexInstall {
        executable: r"C:\npm\codex.cmd".into(),
        version: version.into(),
        runtime_identity: "native-windows".into(),
        codex_home: r"C:\Users\u\.codex".into(),
        wsl_distro: None,
    }
}

fn codex_proxy(advisory: Option<VerificationAdvisory>) -> PreparedLaunchRouting {
    PreparedLaunchRouting::CodexProxy {
        harness_id: "codex".into(),
        provider_id: "minimax".into(),
        account_name: "MiniMax".into(),
        profile_name: "codex-minimax".into(),
        descriptor: crate::agent::provider::compatibility::EndpointModelDescriptor {
            provider_id: "minimax".into(),
            endpoint: "https://api.minimax.io/v1".into(),
            model_id: "MiniMax-M3".into(),
            ..Default::default()
        },
        verification: None,
        advisory,
        runtime: crate::models::EnvType::Windows,
        install: codex_install("0.166.0"),
        credential_reference: codex::PROXY_CREDENTIAL_ENV.into(),
        credential: "sentinel".into(),
    }
}

/// A route that was never checked launches, warns, and is re-checked in the
/// background — the whole point of making the verification advisory.
#[test]
fn an_unchecked_route_warns_and_asks_for_a_background_recheck() {
    let follow_up = codex_proxy(Some(VerificationAdvisory::Missing))
        .verification_follow_up()
        .expect("an unchecked route owes the user a warning");
    let crate::agent::launch_routing::VerificationFollowUp::WarnAndRecheck {
        provider_id,
        message,
    } = follow_up
    else {
        panic!("an unchecked route must ask for a background re-check");
    };
    assert_eq!(provider_id, "minimax");
    assert!(message.contains("MiniMax"), "{message}");
    assert!(message.contains("not been checked"), "{message}");
    assert!(
        message.contains("background"),
        "the warning must say the re-check is already running: {message}"
    );
}

/// A route whose check went stale — a routine `codex-cli` npm update — also
/// launches and is re-checked, and the warning names the CLI change.
#[test]
fn a_stale_route_warns_and_asks_for_a_background_recheck() {
    let follow_up = codex_proxy(Some(VerificationAdvisory::Stale {
        reason: "the Codex CLI changed (0.166.0, previously 0.165.0)".into(),
    }))
    .verification_follow_up()
    .expect("a stale route owes the user a warning");
    let crate::agent::launch_routing::VerificationFollowUp::WarnAndRecheck { message, .. } =
        follow_up
    else {
        panic!("a stale route must ask for a background re-check");
    };
    assert!(message.contains("0.166.0"), "{message}");
    assert!(message.contains("re-checking"), "{message}");
}

/// A route whose check failed launches too — but it is not re-probed on this
/// launch: the check needs a credential or endpoint fix first.
#[test]
fn a_failed_route_warns_without_asking_for_a_recheck() {
    let follow_up = codex_proxy(Some(VerificationAdvisory::Failed {
        reason: "authentication failed (401)".into(),
    }))
    .verification_follow_up()
    .expect("a failed route owes the user a warning");
    let crate::agent::launch_routing::VerificationFollowUp::WarnOnly {
        provider_id,
        message,
    } = follow_up
    else {
        panic!("a failed route must not re-probe a rejecting endpoint");
    };
    assert_eq!(provider_id, "minimax");
    assert!(message.contains("authentication failed"), "{message}");
}

/// A route covered by a current successful check owes the user nothing.
#[test]
fn a_verified_route_has_no_follow_up() {
    assert!(codex_proxy(None).verification_follow_up().is_none());
}
