//! `GET /api/providers` — list providers available on this host.

use crate::agent::provider_menu::available_providers;
use crate::http::read_status::{read_response, serialized};
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::read_outcome::Unavailable;

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
