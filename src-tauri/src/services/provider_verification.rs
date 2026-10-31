//! Live verification for exact proxied provider pairings (issue #1098).

use crate::agent::provider::adapters::codex;
use crate::agent::provider::compatibility::{pairing_signature, PairingSignatureInputs};
use crate::models::EnvType;
use crate::preferences::{
    self, PairingVerification, PairingVerificationStatus, ProviderAccount, ProviderPairing,
};
use crate::services::http_client::blocking_client_builder;
use chrono::Utc;
use std::io::BufRead;

fn responses_url(base_url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(base_url)
        .map_err(|_| "endpoint must be a valid HTTPS URL".to_string())?;
    if parsed.scheme() != "https" && !(cfg!(test) && parsed.scheme() == "http") {
        return Err("endpoint must use HTTPS".into());
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("endpoint must not contain credentials, a query, or a fragment".into());
    }
    Ok(format!("{}/responses", base_url.trim_end_matches('/')))
}

fn classified_http_error(status: reqwest::StatusCode, host: &str) -> String {
    match status.as_u16() {
        401 | 403 => {
            "authentication failed; update the provider credential and verify again".into()
        }
        400 | 404 | 422 => "endpoint rejected the configured model or Responses request".into(),
        _ => format!("provider endpoint {host} returned HTTP {status}"),
    }
}

fn read_sse_events(
    response: reqwest::blocking::Response,
) -> Result<Vec<serde_json::Value>, String> {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
    {
        return Err("Responses endpoint did not return an event stream".into());
    }

    // Read the response as a live byte stream. Do not call `Response::text()`:
    // that buffers the entire body and cannot distinguish an SSE transport
    // from an ordinary completion containing SSE-looking text.
    let mut events = Vec::new();
    for line in std::io::BufReader::new(response).lines() {
        let line = line.map_err(|_| "provider returned an unreadable streaming response")?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            continue;
        }
        let event = serde_json::from_str(data)
            .map_err(|_| "Responses endpoint returned a malformed streaming event")?;
        events.push(event);
    }
    Ok(events)
}

const VERIFICATION_PROMPT: &str = "Call buildmesh_verification once with value 'ready'.";

fn verify_responses_agent_loop(
    descriptor: &crate::agent::provider::compatibility::EndpointModelDescriptor,
    credential: &str,
) -> Result<(), String> {
    let url = responses_url(&descriptor.endpoint)?;
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "provider".into());
    let client = blocking_client_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("could not initialize verification client: {e}"))?;
    let first = client
        .post(&url)
        .bearer_auth(credential)
        .json(&serde_json::json!({
            "model": descriptor.model_id,
            "input": VERIFICATION_PROMPT,
            "stream": true,
            "tools": [{
                "type": "function",
                "name": "buildmesh_verification",
                "description": "Verifies a tool-call round trip",
                "parameters": {
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"],
                    "additionalProperties": false
                },
                // Codex sends strict function tools; exercising the same
                // shape here keeps verification faithful to real traffic.
                "strict": true
            }]
        }))
        .send()
        .map_err(|_| format!("provider endpoint {host} is unavailable"))?;
    if !first.status().is_success() {
        return Err(classified_http_error(first.status(), &host));
    }
    let events = read_sse_events(first)?;
    if events.len() < 2 {
        return Err("Responses endpoint did not produce an incremental event stream".into());
    }
    let call_id = events.iter().find_map(|event| {
        event
            .get("item")
            .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call"))
            .and_then(|item| item.get("call_id"))
            .and_then(|id| id.as_str())
    });
    let call_name = events.iter().find_map(|event| {
        event
            .get("item")
            .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call"))
            .and_then(|item| item.get("name"))
            .and_then(|name| name.as_str())
    });
    // Prefer the terminal `…arguments.done` event: intermediate
    // `output_item.added` events can carry a *partial* arguments string while
    // the provider streams the call incrementally.
    let arguments = events
        .iter()
        .find_map(|event| {
            if event.get("type").and_then(|value| value.as_str())
                == Some("response.function_call_arguments.done")
            {
                event.get("arguments").and_then(|value| value.as_str())
            } else {
                None
            }
        })
        .or_else(|| {
            events.iter().find_map(|event| {
                event
                    .get("item")
                    .and_then(|item| item.get("arguments"))
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
            })
        });
    let (call_id, call_name, arguments) = match (call_id, call_name, arguments) {
        (Some(call_id), Some(call_name), Some(arguments)) => (call_id, call_name, arguments),
        _ => return Err("Responses stream did not produce a tool call".into()),
    };
    let sentinel_is_valid = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("value")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .is_some_and(|value| value == "ready");
    if !sentinel_is_valid {
        return Err("Responses tool call did not return the sentinel arguments".into());
    }

    // The tool result is replayed as explicit input items — the OpenAI
    // Responses multi-turn shape. `previous_response_id` cannot be used here:
    // some Responses implementations (MiniMax's included) reject it with an
    // HTTP 500 instead of ignoring it.
    let second = client
        .post(&url)
        .bearer_auth(credential)
        .json(&serde_json::json!({
            "model": descriptor.model_id,
            "input": [
                { "type": "message", "role": "user", "content": VERIFICATION_PROMPT },
                {
                    "type": "function_call",
                    "call_id": call_id,
                    "name": call_name,
                    "arguments": arguments
                },
                {
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": "verified"
                }
            ],
            "stream": true
        }))
        .send()
        .map_err(|_| format!("provider endpoint {host} became unavailable"))?;
    if !second.status().is_success() {
        return Err(classified_http_error(second.status(), &host));
    }
    let second_events = read_sse_events(second)?;
    let completed = second_events.iter().any(|event| {
        event.get("type").and_then(|value| value.as_str()) == Some("response.completed")
    });
    let produced_completion = second_events.iter().any(|event| {
        matches!(
            event.get("type").and_then(|value| value.as_str()),
            Some("response.output_text.delta") | Some("response.reasoning_summary_text.delta")
        )
    });
    if !completed {
        return Err("Responses stream did not complete after the tool result".into());
    }
    if !produced_completion {
        return Err(
            "Responses stream did not produce text or reasoning after the tool result".into(),
        );
    }
    Ok(())
}

fn pairing_and_account(
    prefs: &preferences::AppPreferences,
    harness_id: &str,
    provider_id: &str,
) -> Result<(ProviderPairing, ProviderAccount), String> {
    let pairing = prefs
        .provider_pairings
        .iter()
        .find(|p| p.harness_id == harness_id && p.provider_id == provider_id)
        .cloned()
        .ok_or_else(|| format!("pairing '{harness_id}:{provider_id}' no longer exists"))?;
    let account = preferences::provider_accounts()
        .into_iter()
        .find(|a| a.id == provider_id)
        .ok_or_else(|| format!("provider '{provider_id}' no longer exists"))?;
    Ok((pairing, account))
}

fn signature_for(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    install: &codex::CodexInstall,
) -> String {
    let descriptor = preferences::endpoint_model_descriptor(pairing);
    pairing_signature(&PairingSignatureInputs {
        harness_id: &pairing.harness_id,
        provider_id: &pairing.provider_id,
        endpoint: &descriptor.endpoint,
        model_id: &descriptor.model_id,
        credential: account.api_key.as_deref().unwrap_or(""),
        auth_mode: "bearer_env",
        runtime: &install.runtime_identity,
        executable: &install.executable,
        codex_version: &install.version,
    })
}

fn save_result(record: PairingVerification) -> Result<PairingVerification, String> {
    preferences::update(|prefs| {
        prefs.pairing_verifications.retain(|existing| {
            existing.harness_id != record.harness_id
                || existing.provider_id != record.provider_id
                || existing.runtime != record.runtime
                || existing.endpoint != record.endpoint
                || existing.model_id != record.model_id
        });
        prefs.pairing_verifications.push(record.clone());
    })?;
    Ok(record)
}

fn incompatible_status(pairing: &ProviderPairing) -> PairingVerificationStatus {
    if pairing.provider_id == "minimax" && pairing.surface == preferences::ApiSurface::OpenAI {
        PairingVerificationStatus::Failed
    } else {
        PairingVerificationStatus::Unsupported
    }
}

pub fn verify_pairing_blocking(
    harness_id: &str,
    provider_id: &str,
    env_type: EnvType,
) -> Result<PairingVerification, String> {
    let prefs = preferences::load()?;
    let (pairing, account) = pairing_and_account(&prefs, harness_id, provider_id)?;
    verify_route_blocking(&pairing, &account, env_type)
}

pub fn verify_route_blocking(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    env_type: EnvType,
) -> Result<PairingVerification, String> {
    let harness_id = pairing.harness_id.as_str();
    let provider_id = pairing.provider_id.as_str();
    let descriptor = preferences::endpoint_model_descriptor(pairing);
    let compatibility = preferences::pairing_compatibility(pairing);
    let mut record = PairingVerification {
        harness_id: harness_id.into(),
        provider_id: provider_id.into(),
        pairing_signature: String::new(),
        endpoint: descriptor.endpoint.clone(),
        model_id: descriptor.model_id.clone(),
        auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
        runtime: codex::runtime_identity(env_type).into(),
        executable: String::new(),
        codex_version: String::new(),
        capability_result: compatibility.clone(),
        status: PairingVerificationStatus::Pending,
        verified_at: None,
        reason: None,
    };
    if !account.enabled {
        record.status = PairingVerificationStatus::Failed;
        record.reason = Some("provider account is disabled".into());
        return save_result(record);
    }
    if !compatibility.compatible {
        record.status = incompatible_status(pairing);
        record.reason = compatibility.reason;
        return save_result(record);
    }
    let credential = account
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| "provider credential is missing".to_string())?;
    let install = codex::discover_supported_install_fresh(env_type)?;
    record.pairing_signature = signature_for(pairing, account, &install);
    record.runtime = install.runtime_identity.clone();
    record.executable = install.executable.clone();
    record.codex_version = install.version.clone();

    let started = std::time::Instant::now();
    match verify_responses_agent_loop(&descriptor, credential) {
        Ok(()) => {
            record.status = PairingVerificationStatus::Verified;
            record.verified_at = Some(Utc::now());
        }
        Err(reason) => {
            record.status = PairingVerificationStatus::Failed;
            record.reason = Some(reason);
        }
    }
    let endpoint_host = reqwest::Url::parse(&descriptor.endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "invalid-endpoint".into());
    tracing::info!(
        harness_id,
        provider_id,
        runtime = codex::log_safe_runtime_identity(&install),
        endpoint_host,
        model_id = descriptor.model_id,
        status = ?record.status,
        duration_ms = started.elapsed().as_millis(),
        "provider pairing verification completed"
    );
    save_result(record)
}

/// [`matching_verification_in`] over the stored verification records — the
/// convenience form for a caller that has no snapshot in hand.
///
/// The Spawn Menu derivation reads preferences once (issue #2532) and hands
/// the records it holds to [`matching_verification_in`], so this loading form
/// has no production caller left; it lives with the test suite that reads it
/// (issue #2450).
#[cfg(test)]
// allow-test-in-production: the loading form of the lookup, kept for the sibling suite
// that reads it (issue #2450).
fn matching_verification(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    install: &codex::CodexInstall,
) -> Option<PairingVerification> {
    let verifications = preferences::load().ok()?.pairing_verifications;
    matching_verification_in(&verifications, pairing, account, install)
}

/// The verification-record lookup over already-loaded records.
///
/// The Spawn Menu derivation reads preferences once (issue #2532), so it hands
/// the records it already holds to this form rather than letting the lookup
/// read them again — a second read inside one derivation is the torn read the
/// snapshot removes.
pub fn matching_verification_in(
    verifications: &[PairingVerification],
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    install: &codex::CodexInstall,
) -> Option<PairingVerification> {
    let expected = signature_for(pairing, account, install);
    let record = verifications
        .iter()
        .filter(|record| {
            record.harness_id == pairing.harness_id
                && record.provider_id == pairing.provider_id
                && record.runtime == install.runtime_identity
        })
        .min_by_key(|record| {
            (record.endpoint != pairing.base_url.as_deref().unwrap_or("")) as u8
                + (record.model_id != pairing.model_tiers.default.as_deref().unwrap_or("")) as u8
        })?
        .clone();
    Some(check_verification(record, pairing, install, &expected))
}

fn check_verification(
    mut record: PairingVerification,
    pairing: &ProviderPairing,
    install: &codex::CodexInstall,
    expected: &str,
) -> PairingVerification {
    // A record whose capability snapshot already failed (e.g. verified against
    // a since-retired model alias) carries its own truthful status and reason —
    // never re-label it as a signature mismatch.
    if !record.capability_result.compatible {
        return record;
    }
    if record.pairing_signature != expected {
        let descriptor = preferences::endpoint_model_descriptor(pairing);
        let routing_changed = record.endpoint.trim_end_matches('/')
            != descriptor.endpoint.trim_end_matches('/')
            || record.model_id != descriptor.model_id;
        let cli_changed =
            record.codex_version != install.version || record.executable != install.executable;
        record.status = PairingVerificationStatus::Stale;
        record.reason = Some(if routing_changed {
            "routing inputs changed; verify the pairing again".into()
        } else if cli_changed {
            format!(
                "the Codex CLI changed ({}, previously {}); verify the pairing again",
                install.version, record.codex_version
            )
        } else {
            "the provider credential changed; verify the pairing again".into()
        });
    }
    record
}

/// Why a Codex proxy route is not covered by a current successful check.
///
/// The pairing verification is **advisory** at launch. A route that has never
/// been checked, whose check went stale (a routine `codex-cli` npm
/// auto-update changes the installed version) and whose check failed all
/// launch — the user decides whether the combination works, and Buildmesh
/// warns them instead of refusing to start a session that may well be fine.
/// The state travels with the route so the spawn can warn the user and
/// re-check in the background while the session runs.
///
/// `an_unchecked_route_launches_with_a_missing_advisory`,
/// `a_stale_route_after_a_cli_update_launches_and_names_the_cli_change` and
/// `a_route_whose_check_failed_launches_and_keeps_the_failure_reason` pin
/// the three states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationAdvisory {
    /// No check has ever run for this route on this runtime.
    Missing,
    /// A check ran, but the inputs it covered have changed since.
    Stale { reason: String },
    /// A check ran and the route did not work.
    Failed { reason: String },
}

impl VerificationAdvisory {
    /// The warning shown when a session starts on this route.
    ///
    /// The copy names the reason. "It might not work" is not something the
    /// user can act on, and the reason is what tells them whether the session
    /// that just started is worth trusting.
    pub fn warning(&self, account: &str, runtime: &str) -> String {
        match self {
            Self::Missing => format!(
                "{account} on {runtime} has not been checked yet — starting anyway; Buildmesh is verifying it in the background."
            ),
            Self::Stale { reason } => format!(
                "{account} on {runtime} was checked against older settings — {reason}. Starting anyway; Buildmesh is re-checking it in the background."
            ),
            Self::Failed { reason } => format!(
                "{account} on {runtime} failed its last check — {reason}. Starting anyway."
            ),
        }
    }

    /// Whether a background re-check can still change this route's outcome.
    ///
    /// An unchecked or stale route is what a re-check resolves. A failed
    /// check needs a credential or endpoint fix first, so probing it again on
    /// each launch would only repeat a request the endpoint has already
    /// rejected.
    pub fn wants_recheck(&self) -> bool {
        matches!(self, Self::Missing | Self::Stale { .. })
    }
}

/// The stored reason without the "verify the pairing again" advice.
///
/// That advice belongs to Settings, where the user chooses to re-verify. The
/// launch warning already says a re-check is running, so repeating it there
/// would read as advice to do the one thing the spawn is already doing.
fn reason_without_advice(reason: &str) -> &str {
    reason
        .strip_suffix("; verify the pairing again")
        .unwrap_or(reason)
}

/// A Codex proxy route that is materially complete: the account is enabled,
/// the credential is present, the endpoint resolves, the capability contract
/// is compatible, and a Codex install was found for the runtime.
///
/// The pairing *verification* is deliberately not part of this. It is
/// advisory — [`CodexProxyRoute::advisory`] carries the state of the last
/// check so the spawn can warn the user and re-check in the background,
/// instead of refusing to start a session that may well work.
pub struct CodexProxyRoute {
    pub descriptor: crate::agent::provider::compatibility::EndpointModelDescriptor,
    pub credential: String,
    /// The check that covers this route, when one exists on this runtime.
    pub verification: Option<PairingVerification>,
    /// Set when the route is not covered by a current successful check.
    pub advisory: Option<VerificationAdvisory>,
    pub install: codex::CodexInstall,
}

/// The error a declared capability-contract incompatibility produces.
///
/// Shared by the two entry points so the spawn path and the Spawn Menu
/// derivation cannot drift on the wording of a failure the user has to act on.
fn unsupported_reason(pairing: &ProviderPairing, account: &ProviderAccount) -> Option<String> {
    let compatibility = preferences::pairing_compatibility(pairing);
    if compatibility.compatible {
        return None;
    }
    Some(format!(
        "pairing '{}' is unsupported: {}",
        account.name,
        compatibility
            .reason
            .unwrap_or_else(|| "incompatible capability contract".into())
    ))
}

/// The hard, route-completeness checks a Codex proxy route must pass.
///
/// These are the failures trying again cannot fix: a disabled account, a
/// missing credential, an endpoint that does not resolve, and a declared
/// capability-contract incompatibility. The pairing verification is NOT one of
/// them — see [`CodexProxyRoute`].
fn codex_route_inputs(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
) -> Result<
    (
        crate::agent::provider::compatibility::EndpointModelDescriptor,
        String,
    ),
    String,
> {
    if !account.enabled {
        return Err(format!("provider '{}' is disabled", account.name));
    }
    let credential = account
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| format!("provider '{}' has no credential", account.name))?;
    let descriptor = preferences::endpoint_model_descriptor(pairing);
    responses_url(&descriptor.endpoint)
        .map_err(|reason| format!("pairing '{}': {reason}", account.name))?;
    if let Some(reason) = unsupported_reason(pairing, account) {
        return Err(reason);
    }
    Ok((descriptor, credential.to_string()))
}

/// Single authority for Codex launch readiness. Spawn-option generation
/// projects this result to a boolean; final preflight preserves its reason.
pub fn codex_proxy_route(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    env_type: EnvType,
) -> Result<CodexProxyRoute, String> {
    // Reject declared protocol/model incompatibility before touching the
    // filesystem, invoking Codex, or considering the endpoint.
    if let Some(reason) = unsupported_reason(pairing, account) {
        return Err(reason);
    }
    let install = codex::discover_supported_install(env_type)?;
    let verifications = preferences::load()
        .map(|prefs| prefs.pairing_verifications)
        .unwrap_or_default();
    codex_proxy_route_with_install(pairing, account, &install, &verifications)
}

fn codex_proxy_route_with_install(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    install: &codex::CodexInstall,
    verifications: &[PairingVerification],
) -> Result<CodexProxyRoute, String> {
    codex_proxy_route_from_record(pairing, account, install, None, verifications)
}

pub fn codex_proxy_route_snapshot(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    env_type: EnvType,
    record: Option<&PairingVerification>,
) -> Result<CodexProxyRoute, String> {
    let install = codex::discover_supported_install(env_type)?;
    let verifications = preferences::load()
        .map(|prefs| prefs.pairing_verifications)
        .unwrap_or_default();
    codex_proxy_route_from_record(pairing, account, &install, record, &verifications)
}

fn codex_proxy_route_from_record(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    install: &codex::CodexInstall,
    snapshot: Option<&PairingVerification>,
    verifications: &[PairingVerification],
) -> Result<CodexProxyRoute, String> {
    let (descriptor, credential) = codex_route_inputs(pairing, account)?;
    let live = matching_verification_in(verifications, pairing, account, install);
    // The record that covers this route: a current success when one exists,
    // otherwise the closest record we hold, so the warning can name what the
    // last check actually said.
    //
    // The live record wins over the plan's snapshot because `live` is already
    // filtered to this runtime — a snapshot captured on another runtime would
    // be re-checked against this install and report *its* CLI against *this*
    // one ("the Codex CLI changed (0.166.0, previously 0.166.0)"). The
    // snapshot is the fallback for a route whose record is no longer in
    // preferences, and only when it was captured on this runtime.
    let verification = live
        .as_ref()
        .filter(|record| record.status == PairingVerificationStatus::Verified)
        .cloned()
        .or_else(|| live.clone())
        .or_else(|| {
            snapshot
                .filter(|record| record.runtime == install.runtime_identity)
                .cloned()
                .map(|record| {
                    check_verification(
                        record,
                        pairing,
                        install,
                        &signature_for(pairing, account, install),
                    )
                })
        });
    // No record at all means the route has never been checked on this
    // runtime — an unproven route, not a proven-good one.
    let advisory = match verification.as_ref() {
        Some(record) => advisory_for(record),
        None => Some(VerificationAdvisory::Missing),
    };
    Ok(CodexProxyRoute {
        descriptor,
        credential,
        verification,
        advisory,
        install: install.clone(),
    })
}

/// The advisory state of a record that is not a current success.
fn advisory_for(record: &PairingVerification) -> Option<VerificationAdvisory> {
    match record.status {
        PairingVerificationStatus::Verified => None,
        PairingVerificationStatus::Stale => Some(VerificationAdvisory::Stale {
            reason: reason_without_advice(
                record
                    .reason
                    .as_deref()
                    .unwrap_or("the checked inputs changed"),
            )
            .to_string(),
        }),
        PairingVerificationStatus::Failed => Some(VerificationAdvisory::Failed {
            reason: reason_without_advice(
                record.reason.as_deref().unwrap_or("the last check failed"),
            )
            .to_string(),
        }),
        // A Pending or Unsupported record carries no verdict of its own: the
        // route is simply not covered by a check yet.
        PairingVerificationStatus::Pending | PairingVerificationStatus::Unsupported => {
            Some(VerificationAdvisory::Missing)
        }
    }
}

pub fn launchable_on_runtime(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    env_type: EnvType,
    codex_install: Option<&codex::CodexInstall>,
) -> bool {
    let verifications = preferences::load()
        .map(|prefs| prefs.pairing_verifications)
        .unwrap_or_default();
    launchable_on_runtime_with(pairing, account, env_type, codex_install, &verifications)
}

/// [`launchable_on_runtime`] over already-loaded verification records.
///
/// The Spawn Menu derivation reads preferences once (issue #2532), so it hands
/// the records the snapshot already holds to this form rather than letting the
/// lookup read them again.
///
/// The records are NOT part of the answer. A route that was never checked,
/// whose check went stale or whose check failed still launches — the user
/// decides whether it works — so gating the row on the verification state
/// would hide a route that is perfectly usable. The records drive
/// [`route_advisory_with`] instead, which is what the menu renders.
pub fn launchable_on_runtime_with(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    _env_type: EnvType,
    codex_install: Option<&codex::CodexInstall>,
    _verifications: &[PairingVerification],
) -> bool {
    if pairing.surface == preferences::ApiSurface::Anthropic {
        return account.enabled
            && account
                .api_key
                .as_deref()
                .is_some_and(|credential| !credential.trim().is_empty())
            && preferences::pairing_compatibility(pairing).compatible;
    }
    // A Codex route needs an install to run and a materially complete account
    // to talk to. The pairing verification is not part of this.
    codex_install.is_some() && codex_route_inputs(pairing, account).is_ok()
}

/// The advisory state of a route over already-loaded verification records.
///
/// `None` means the route is covered by a current successful check — or that
/// it is not a Codex route at all, which carries no verification of its own.
pub fn route_advisory_with(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    codex_install: Option<&codex::CodexInstall>,
    verifications: &[PairingVerification],
) -> Option<VerificationAdvisory> {
    if pairing.surface == preferences::ApiSurface::Anthropic {
        return None;
    }
    let install = codex_install?;
    // No record at all means the route has never been checked on this
    // runtime — an unproven route, not a proven-good one.
    let Some(record) = matching_verification_in(verifications, pairing, account, install) else {
        return Some(VerificationAdvisory::Missing);
    };
    advisory_for(&record)
}

pub fn current_statuses(env_type: EnvType) -> Vec<PairingVerification> {
    // Issue #1937: the Settings → Providers tab calls this twice per load
    // (once per runtime) on top of `available_providers`, so each call emits
    // one `info` line (lands in `logs\buildmesh.log` on a default install)
    // with the Codex probe cost. Only the runtime identity and CLI version
    // travel in the fields — never credentials, keys, or endpoint URLs.
    let derivation_started = std::time::Instant::now();
    let prefs = preferences::load().unwrap_or_default();
    let accounts = preferences::provider_accounts();
    let probe_started = std::time::Instant::now();
    let install = codex::discover_supported_install(env_type);
    let probe_duration_ms = probe_started.elapsed().as_millis();
    let statuses: Vec<PairingVerification> = prefs
        .provider_pairings
        .iter()
        .filter_map(|pairing| {
            let account = accounts
                .iter()
                .find(|account| account.id == pairing.provider_id)?;
            let descriptor = preferences::endpoint_model_descriptor(pairing);
            let decision = preferences::pairing_compatibility(pairing);
            let base = || PairingVerification {
                harness_id: pairing.harness_id.clone(),
                provider_id: pairing.provider_id.clone(),
                pairing_signature: String::new(),
                endpoint: descriptor.endpoint.clone(),
                model_id: descriptor.model_id.clone(),
                auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
                runtime: codex::runtime_identity(env_type).into(),
                executable: String::new(),
                codex_version: String::new(),
                capability_result: decision.clone(),
                status: PairingVerificationStatus::Pending,
                verified_at: None,
                reason: Some("verification has not completed".into()),
            };
            if !decision.compatible {
                let mut record = base();
                record.status = incompatible_status(pairing);
                record.reason = decision.reason;
                return Some(record);
            }
            if pairing.surface == preferences::ApiSurface::Anthropic {
                let mut record = base();
                record.status = PairingVerificationStatus::Verified;
                record.reason = None;
                return Some(record);
            }
            match &install {
                Ok(install) => matching_verification_in(
                    &prefs.pairing_verifications,
                    pairing,
                    account,
                    install,
                )
                .or_else(|| Some(base())),
                Err(reason) => {
                    let mut record = base();
                    record.status = PairingVerificationStatus::Failed;
                    record.reason = Some(reason.clone());
                    Some(record)
                }
            }
        })
        .collect();
    tracing::info!(
        runtime = codex::runtime_identity(env_type),
        codex_version = install
            .as_ref()
            .map(|install| install.version.as_str())
            .unwrap_or("none"),
        probe_duration_ms,
        pairing_count = statuses.len(),
        total_duration_ms = derivation_started.elapsed().as_millis(),
        "pairing verification statuses completed"
    );
    statuses
}

#[cfg(test)]
#[path = "provider_verification_tests.rs"]
mod tests;
