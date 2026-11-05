use super::*;

#[tokio::test]
async fn label_route_rejects_malformed_or_invalid_writes_before_calling_github() {
    for body in [
        r#"{}"#,
        r#"{"label":"bug","present":"yes"}"#,
        r#"{"label":" ","present":true}"#,
        r#"{"label":"bug","present":true,"labels":[]}"#,
    ] {
        let req = ParsedRequest::test_post("/api/meshes/3/issues/42/label", body.as_bytes())
            .with_ids(Some(3), Some(42));
        assert_eq!(set_label(&req).await.status_code(), 400);
    }
    let req = ParsedRequest::test_post(
        "/api/meshes/3/issues/0/label",
        br#"{"label":"bug","present":true}"#,
    )
    .with_ids(Some(3), Some(0));
    assert_eq!(set_label(&req).await.status_code(), 400);
}

#[tokio::test]
async fn label_route_acknowledges_single_label_add_and_remove() {
    for present in [true, false] {
        let body =
            serde_json::to_vec(&serde_json::json!({"label": "buildmesh:run", "present": present}))
                .unwrap();
        let req = ParsedRequest::test_post("/api/meshes/3/issues/42/label", &body)
            .with_ids(Some(3), Some(42));
        let response = set_label_with(&req, |mesh, issue, label, checked| async move {
            assert_eq!(
                (mesh, issue, label.as_str(), checked),
                (3, 42, "buildmesh:run", present)
            );
            Ok(())
        })
        .await;
        assert_eq!(response.status_code(), 204);
        assert!(response.body().is_empty());
    }
}

#[tokio::test]
async fn github_write_failure_is_not_a_device_authorization_failure_or_success() {
    let req = ParsedRequest::test_post(
        "/api/meshes/3/issues/42/label",
        br#"{"label":"bug","present":true}"#,
    )
    .with_ids(Some(3), Some(42));
    let response = set_label_with(&req, |_, _, _, _| async {
        Err("GitHub permission denied".into())
    })
    .await;
    assert_eq!(response.status_code(), 500);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(response.body()).unwrap()["error"],
        "GitHub permission denied"
    );
}

fn issue_spawn_request(mesh_id: i64, issue: i64, body: &str) -> ParsedRequest {
    ParsedRequest::test_post(
        &format!("/api/meshes/{mesh_id}/issues/{issue}/spawn"),
        body.as_bytes(),
    )
    .with_ids(Some(mesh_id), Some(issue))
}

async fn never_create(_: i64, _: i64, _: SpawnRequest) -> Result<serde_json::Value, String> {
    panic!("a rejected body must not create a node")
}

#[tokio::test]
async fn issue_spawn_rejects_the_legacy_body_shape_and_non_positive_ids() {
    for (mesh, issue, body) in [
        (
            3,
            42,
            r#"{"title":"t","body":"full issue text","provider":"claude"}"#,
        ),
        (3, 42, r#"{"provider":"claude"}"#),
        (3, 0, r#"{"title":"t"}"#),
        (0, 42, r#"{"title":"t"}"#),
    ] {
        let response = spawn_with(&issue_spawn_request(mesh, issue, body), never_create).await;
        assert_eq!(response.status_code(), 400, "body {body} on {mesh}/{issue}");
    }
}

#[tokio::test]
async fn issue_spawn_keeps_the_original_body_and_accepts_a_saved_configuration() {
    for (body, expected) in [
        (
            r#"{"title":"Fix deadline","provider":"claude"}"#,
            SpawnRequest {
                title: "Fix deadline".into(),
                provider: Some("claude".into()),
                configuration_id: None,
                login_id: None,
            },
        ),
        (
            r#"{"title":"Fix deadline"}"#,
            SpawnRequest {
                title: "Fix deadline".into(),
                provider: None,
                configuration_id: None,
                login_id: None,
            },
        ),
        (
            r#"{"title":"Fix deadline","provider":"claude","configuration_id":"launch/saved","login_id":"default"}"#,
            SpawnRequest {
                title: "Fix deadline".into(),
                provider: Some("claude".into()),
                configuration_id: Some("launch/saved".into()),
                login_id: Some("default".into()),
            },
        ),
    ] {
        let response = spawn_with(&issue_spawn_request(3, 42, body), |mesh, issue, request| {
            let expected = expected.clone();
            async move {
                assert_eq!((mesh, issue, request), (3, 42, expected));
                Ok(serde_json::json!({"id": 17, "status": "pending"}))
            }
        })
        .await;
        assert_eq!(response.status_code(), 200, "body {body}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(response.body()).unwrap(),
            serde_json::json!({"id": 17, "status": "pending"})
        );
    }
}

#[tokio::test]
async fn issue_spawn_creation_failure_is_reported_with_its_message() {
    let response = spawn_with(
        &issue_spawn_request(3, 42, r#"{"title":"t"}"#),
        |_, _, _| async { Err::<serde_json::Value, _>("Unknown launch configuration".to_string()) },
    )
    .await;
    assert_eq!(response.status_code(), 500);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(response.body()).unwrap()["error"],
        "Unknown launch configuration"
    );
}

#[test]
fn catalog_identifies_only_enabled_issue_triggers_for_this_mesh() {
    let circuit =
        |id, mesh_id, enabled, name: &str, graph_json: &str| crate::models::AutopilotCircuit {
            id,
            mesh_id,
            enabled,
            name: name.into(),
            graph_json: graph_json.into(),
            description: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            is_preset: false,
        };
    let graph = r#"{"version":1,"nodes":[{"id":"issue","type":{"type":"github_issue_label","label":"Buildmesh:Run"}},{"id":"again","type":{"type":"github_issue_label","label":"buildmesh:run"}},{"id":"pr","type":{"type":"github_pull_request_label","label":"review"}}],"edges":[]}"#;
    let triggers = label_triggers(
        3,
        vec![
            circuit(1, 3, true, "Ship fixes", graph),
            circuit(2, 3, false, "Disabled", graph),
            circuit(3, 4, true, "Other mesh", graph),
            circuit(4, 3, true, "Broken", "invalid"),
            circuit(5, 3, true, "Second circuit", graph),
        ],
    );
    let catalog = serde_json::to_value(IssueLabelCatalog {
        labels: vec!["bug".into(), "Buildmesh:Run".into()],
        autopilot_triggers: triggers,
    })
    .unwrap();
    assert_eq!(
        catalog,
        serde_json::json!({"labels":["bug","Buildmesh:Run"],"autopilot_triggers":{"buildmesh:run":["Ship fixes","Second circuit"]}})
    );
}
