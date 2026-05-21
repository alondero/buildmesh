//! `GET /api/providers` — list providers available on this host.

use crate::commands::agent::available_providers;

pub fn list_json() -> String {
    serde_json::to_string(&available_providers()).unwrap_or_else(|_| "[]".to_string())
}
