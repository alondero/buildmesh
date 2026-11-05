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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) login_id: Option<String>,
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
                body.login_id,
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
#[path = "issues_tests.rs"]
mod tests;
