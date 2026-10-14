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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelRequest {
    label: String,
    present: bool,
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
struct SpawnRequest {
    title: String,
    provider: Option<String>,
}

pub async fn spawn(req: &ParsedRequest) -> Response {
    let mesh_id = req.id0();
    let issue_number = req.id1();

    let parsed: SpawnRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    let Some(app) = state::app_handle() else {
        return Response::json_error("503 Service Unavailable", "App not ready");
    };

    // spawn_issue_agent is a #[tauri::command] but takes plain args except
    // for AppHandle which we already hold, so we call it directly. The
    // backend derives the GitHub URL from the mesh's `origin` remote — we
    // only need the issue number and a title hint here.
    match crate::commands::agent::spawn_issue_agent(
        app.clone(),
        mesh_id,
        issue_number,
        parsed.title,
        parsed.provider,
    )
    .await
    {
        Ok(node) => {
            let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());
            Response::json("200 OK", body)
        }
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
