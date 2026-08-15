//! `GET /api/providers` — list providers available on this host.

use crate::commands::agent::available_providers;

pub async fn list_json() -> String {
    let providers = crate::commands::run_blocking("http_list_providers", || {
        Ok(available_providers())
    })
    .await
    .unwrap_or_default();
    serde_json::to_string(&providers).unwrap_or_else(|_| "[]".to_string())
}
