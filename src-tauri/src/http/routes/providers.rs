//! `GET /api/providers` — list providers available on this host.

use crate::agent::provider_menu::available_providers;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;

pub async fn list(_req: &ParsedRequest) -> Response {
    Response::json("200 OK", list_json().await)
}

pub async fn list_json() -> String {
    let providers = crate::commands::run_blocking("http_list_providers", || {
        Ok(available_providers())
    })
    .await
    .unwrap_or_default();
    serde_json::to_string(&providers).unwrap_or_else(|_| "[]".to_string())
}
