use super::launch::LaunchParams;
use super::reader::SessionIdMode;

/// Construction pin: these knobs belong to launch, not provision.
#[test]
fn launch_params_carry_pty_size_and_cascade_overrides() {
    let launch = LaunchParams {
        rows: 24,
        cols: 80,
        prefill: Some("hello".into()),
        explicit_model: Some("sonnet-4".into()),
        explicit_effort: Some("low".into()),
        explicit_extra_args: Some("--verbose".into()),
        harness_id: "anthropic".into(),
        node_mesh_id: 1,
        registry_mesh_id: 1,
        session_id_mode: SessionIdMode::None,
        sandbox: false,
    };
    assert_eq!(launch.rows, 24);
    assert_eq!(launch.cols, 80);
    assert_eq!(launch.prefill.as_deref(), Some("hello"));
    assert_eq!(launch.explicit_model.as_deref(), Some("sonnet-4"));
    assert_eq!(launch.explicit_effort.as_deref(), Some("low"));
    assert_eq!(launch.explicit_extra_args.as_deref(), Some("--verbose"));
}

#[test]
fn provisioned_workspace_has_no_launch_knobs() {
    let src = include_str!("provision.rs");
    let start = src
        .find("pub(super) struct ProvisionedWorkspace")
        .expect("ProvisionedWorkspace must exist");
    let rest = &src[start..];
    let end = rest
        .find("pub(super) fn run_provider_provisioning")
        .unwrap_or(rest.len());
    let body = &rest[..end];
    for needle in [
        "rows",
        "cols",
        "prefill",
        "explicit_model",
        "explicit_effort",
        "explicit_extra_args",
    ] {
        assert!(
            !body.contains(needle),
            "ProvisionedWorkspace must not courier launch knob {needle}"
        );
    }
}

#[test]
fn launch_process_logs_under_its_own_name() {
    let src = include_str!("launch.rs");
    assert!(
        src.contains("launch_process: process spawned successfully"),
        "launch failures must be greppable as launch_process"
    );
    assert!(
        !src.contains("spawn_agent_inner:"),
        "launch.rs must not log as the old monolith"
    );
}

#[test]
fn launch_process_accepts_provisioned_workspace_and_launch_params() {
    let src = include_str!("launch.rs");
    let sig = src
        .split("pub(super) async fn launch_process")
        .nth(1)
        .expect("launch_process must exist");
    let header = &sig[..sig.find('{').expect("fn body")];
    assert!(
        header.contains("provisioned: ProvisionedWorkspace"),
        "launch_process takes the provisioned workspace"
    );
    assert!(
        header.contains("launch: LaunchParams"),
        "launch_process takes launch params directly, not couriered through provision"
    );
}
