//! `GET /api/providers` — list providers available on this host.

use crate::agent::provider_menu::available_providers;
use crate::http::read_status::{read_response, serialized};
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::preferences::ProviderAccount;
use crate::read_outcome::Unavailable;
use crate::services::usage::ProviderMeters;
use tauri::Manager;

/// Read-only account identity and usage. ProviderAccount itself contains secrets.
#[derive(serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "MobileProviderUsage.ts")]
pub struct MobileProviderUsage {
    pub name: String,
    pub meter: ProviderMeters,
}

fn usage_rows(
    accounts: &[ProviderAccount],
    meters: Vec<ProviderMeters>,
) -> Vec<MobileProviderUsage> {
    meters
        .into_iter()
        .filter_map(|meter| {
            let account = accounts
                .iter()
                .find(|a| a.id == meter.provider && a.enabled)?;
            Some(MobileProviderUsage {
                name: account.name.clone(),
                meter,
            })
        })
        .collect()
}

pub async fn usage(req: &ParsedRequest) -> Response {
    let force = match req.query_param("refresh").as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        _ => return Response::json_error("400 Bad Request", "refresh must be true or false"),
    };
    let Some(app) = crate::http::app_handle() else {
        return Response::json_error("503 Service Unavailable", "Desktop is not ready");
    };
    let accounts = match crate::commands::run_blocking("http_usage_accounts", || {
        Ok(crate::preferences::provider_accounts())
    })
    .await
    {
        Ok(accounts) => accounts,
        Err(error) => return Response::json_error("503 Service Unavailable", &error),
    };
    match crate::commands::usage::get_provider_meters(force, app.state()).await {
        Ok(meters) => match serialized("200 OK", &usage_rows(&accounts, meters)) {
            Ok(response) => response,
            Err(error) => Response::json_error("503 Service Unavailable", &error),
        },
        Err(error) => Response::json_error("503 Service Unavailable", &error),
    }
}

pub async fn list(_req: &ParsedRequest) -> Response {
    match list_json().await {
        Ok(response) => response,
        // A list read is never "absent" — this message only names the
        // resource the list is *of*, for the (unreachable) absence branch.
        Err(failure) => read_response(&failure, "No providers found"),
    }
}

/// The host's provider menu as a JSON body.
///
/// Issue #2330: a failed offload used to become `.unwrap_or_default()` — an
/// empty menu under a `200`. A phone rendering "no providers available" from
/// that would send the user to the settings screen for a harness that is
/// installed, so the offload failure is now reported as unavailable.
pub async fn list_json() -> Result<Response, Unavailable> {
    let providers =
        crate::commands::run_blocking_typed::<_, Unavailable, _>("http_list_providers", || {
            Ok(available_providers())
        })
        .await?;

    serialized("200 OK", &providers).map_err(Unavailable::Read)
}

#[cfg(test)]
#[path = "providers_tests.rs"]
mod usage_tests;
