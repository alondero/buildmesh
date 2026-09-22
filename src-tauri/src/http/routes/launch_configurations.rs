use crate::http::{response::Response, router::ParsedRequest};
use crate::preferences::{launch_catalog, spawn_configurations};

pub async fn list(_: &ParsedRequest) -> Response {
    respond(crate::commands::run_blocking("list_launch_configurations", || {
        serde_json::to_string(&spawn_configurations::list_spawn_configurations()?).map_err(|e| e.to_string())
    }).await)
}

pub async fn targets(_: &ParsedRequest) -> Response {
    respond(crate::commands::run_blocking("launch_targets", || {
        serde_json::to_string(&launch_catalog::get_launch_targets()?).map_err(|e| e.to_string())
    }).await)
}

pub async fn save(req: &ParsedRequest) -> Response {
    let value = match serde_json::from_slice::<spawn_configurations::SpawnConfiguration>(&req.body) {
        Ok(value) => value,
        Err(error) => return Response::json_error("400 Bad Request", &error.to_string()),
    };
    respond(crate::commands::run_blocking("save_launch_configuration", move || {
        let value = spawn_configurations::save_value(value)?;
        notify();
        serde_json::to_string(&value).map_err(|e| e.to_string())
    }).await)
}

pub async fn delete(req: &ParsedRequest) -> Response {
    #[derive(serde::Deserialize)]
    struct Delete { id: String }
    let value = match serde_json::from_slice::<Delete>(&req.body) {
        Ok(value) => value,
        Err(error) => return Response::json_error("400 Bad Request", &error.to_string()),
    };
    respond(crate::commands::run_blocking("delete_launch_configuration", move || {
        spawn_configurations::delete_value(&value.id)?;
        notify();
        Ok("{}".into())
    }).await)
}

fn notify() {
    use tauri::Emitter;
    if let Some(app) = crate::http::app_handle() { let _ = app.emit("provider-list-changed", ()); }
}

fn respond(result: Result<String, String>) -> Response {
    match result { Ok(body) => Response::json("200 OK", body), Err(error) => Response::json_error("400 Bad Request", &error) }
}
