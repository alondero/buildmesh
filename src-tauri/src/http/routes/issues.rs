//! GitHub issue browsing + issue-driven agent spawning.

use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::state;

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "IssueLabelCatalog.ts")]
pub struct IssueLabelCatalog {
    pub labels: Vec<String>,
    pub autopilot_triggers: std::collections::BTreeMap<String, Vec<String>>,
}

pub async fn labels(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
    let result = async {
        let labels = crate::commands::github::get_repo_labels(mesh_id).await?;
        let autopilot_triggers = crate::commands::run_blocking("issue_label_triggers", move || {
            Ok(label_triggers(
                mesh_id,
                crate::commands::circuit::list_circuits(mesh_id)?,
            ))
        })
        .await?;
        serde_json::to_string(&IssueLabelCatalog {
            labels,
            autopilot_triggers,
        })
        .map_err(|e| e.to_string())
    }
    .await;
    match result {
        Ok(body) => Response::json("200 OK", body),
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

fn label_triggers(
    mesh_id: i64,
    circuits: Vec<crate::models::AutopilotCircuit>,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut triggers = std::collections::BTreeMap::<String, Vec<String>>::new();
    for circuit in circuits
        .into_iter()
        .filter(|c| c.enabled && c.mesh_id == mesh_id)
    {
        let Ok(graph) =
            serde_json::from_str::<crate::circuit::model::CircuitGraph>(&circuit.graph_json)
        else {
            continue;
        };
        for node in graph.nodes {
            if let crate::circuit::model::CircuitNodeKind::GithubIssueLabel { label } = node.kind {
                let names = triggers.entry(label.to_lowercase()).or_default();
                if !names.contains(&circuit.name) {
                    names.push(circuit.name.clone());
                }
            }
        }
    }
    triggers
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LabelRequest {
    pub(crate) label: String,
    pub(crate) present: bool,
}

pub async fn set_label(req: &ParsedRequest) -> Response {
    set_label_with(req, crate::commands::github::set_issue_label).await
}

async fn set_label_with<F, Fut>(req: &ParsedRequest, write: F) -> Response
where
    F: FnOnce(i64, i64, String, bool) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let parsed: LabelRequest = match serde_json::from_slice(&req.body) {
        Ok(value) => value,
        Err(e) => return Response::json_error("400 Bad Request", &format!("Invalid JSON: {e}")),
    };
    if req.id0() <= 0 || req.id1() <= 0 || parsed.label.trim().is_empty() {
        return Response::json_error(
            "400 Bad Request",
            "A positive mesh and issue number and non-empty label are required",
        );
    }
    match write(req.id0(), req.id1(), parsed.label, parsed.present).await {
        Ok(()) => Response::empty("204 No Content"),
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

pub async fn list(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
    // Await the async command wrapper (not the `*_blocking` core): this route
    // runs inside `tauri::async_runtime::spawn` (http/server.rs), so calling the
    // blocking core directly would park a Tauri worker. The wrapper offloads to
    // the blocking pool via `run_blocking`.
    match crate::commands::pr::get_repo_issues(mesh_id).await {
        Ok(issues) => {
            let body = serde_json::to_string(&issues).unwrap_or_else(|_| "[]".to_string());
            Response::json("200 OK", body)
        }
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
// deny_unknown_fields is deliberate. The previous SpawnRequest carried a
// `body: String`; if a stale mobile bundle (served from `dist/mobile` via
// rust-embed and possibly cached on a phone for releases) keeps POSTing the
// legacy `{title, body, provider}` shape, we want a loud 400 — not a silent
// body-dropped success — so the user knows to refresh. See memory:
// buildmesh-serde-default-fragility.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct SpawnRequest {
    pub(crate) title: String,
    /// Spawn Option; absent means the mesh default.
    #[serde(default)]
    pub(crate) provider: Option<String>,
    /// Saved Launch Configuration under `provider`.
    #[serde(default)]
    pub(crate) configuration_id: Option<String>,
}

/// `POST /api/meshes/{id}/issues/{n}/spawn` — the desktop Issues probe's
/// `create_issue_node`: the node row is committed and returned at once, and
/// the worktree and process start in the background, reporting through the
/// node's status. The backend derives the issue URL from the mesh's `origin`
/// remote and renders the prompt, so the body carries only a title hint.
pub async fn spawn(req: &ParsedRequest) -> Response {
    let Some(app) = state::app_handle() else {
        return Response::json_error("503 Service Unavailable", "App not ready");
    };
    spawn_with(req, |mesh_id, issue_number, body| async move {
        crate::commands::run_blocking("http_spawn_issue", move || {
            crate::commands::agent::create_issue_node(
                app.clone(),
                mesh_id,
                issue_number,
                body.title,
                body.provider,
                body.configuration_id,
            )
            .map(|draft| draft.node)
        })
        .await
    })
    .await
}

async fn spawn_with<F, Fut, T>(req: &ParsedRequest, create: F) -> Response
where
    F: FnOnce(i64, i64, SpawnRequest) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
    T: serde::Serialize,
{
    let parsed: SpawnRequest = match serde_json::from_slice(&req.body) {
        Ok(value) => value,
        Err(e) => return Response::json_error("400 Bad Request", &format!("Invalid JSON: {e}")),
    };
    if req.id0() <= 0 || req.id1() <= 0 {
        return Response::json_error(
            "400 Bad Request",
            "A positive mesh and issue number are required",
        );
    }
    match create(req.id0(), req.id1(), parsed).await {
        Ok(node) => match serde_json::to_string(&node) {
            Ok(body) => Response::json("200 OK", body),
            Err(e) => Response::json_error("500 Internal Server Error", &e.to_string()),
        },
        Err(e) => Response::json_error("500 Internal Server Error", &e),
    }
}

#[cfg(test)]
mod tests {
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
            let body = serde_json::to_vec(
                &serde_json::json!({"label": "buildmesh:run", "present": present}),
            )
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
                },
            ),
            (
                r#"{"title":"Fix deadline"}"#,
                SpawnRequest {
                    title: "Fix deadline".into(),
                    provider: None,
                    configuration_id: None,
                },
            ),
            (
                r#"{"title":"Fix deadline","provider":"claude","configuration_id":"launch/saved"}"#,
                SpawnRequest {
                    title: "Fix deadline".into(),
                    provider: Some("claude".into()),
                    configuration_id: Some("launch/saved".into()),
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
            |_, _, _| async {
                Err::<serde_json::Value, _>("Unknown launch configuration".to_string())
            },
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
}
