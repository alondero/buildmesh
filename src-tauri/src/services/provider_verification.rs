//! Live verification for exact proxied provider pairings (issue #1098).

use crate::agent::provider::adapters::codex;
use crate::agent::provider::compatibility::{pairing_signature, PairingSignatureInputs};
use crate::models::EnvType;
use crate::preferences::{
    self, PairingVerification, PairingVerificationStatus, ProviderAccount, ProviderPairing,
};
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
        401 | 403 => "authentication failed; update the provider credential and verify again".into(),
        400 | 404 | 422 => "endpoint rejected the configured model or Responses request".into(),
        _ => format!("provider endpoint {host} returned HTTP {status}"),
    }
}

fn read_sse_events(response: reqwest::blocking::Response) -> Result<Vec<serde_json::Value>, String> {
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

fn verify_responses_agent_loop(
    descriptor: &crate::agent::provider::compatibility::EndpointModelDescriptor,
    credential: &str,
) -> Result<(), String> {
    let url = responses_url(&descriptor.endpoint)?;
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "provider".into());
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("could not initialize verification client: {e}"))?;
    let first = client
        .post(&url)
        .bearer_auth(credential)
        .json(&serde_json::json!({
            "model": descriptor.model_id,
            "input": "Call buildmesh_verification once with value 'ready'.",
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
    let response_id = events.iter().find_map(|event| {
        event
            .get("response")
            .and_then(|r| r.get("id"))
            .and_then(|id| id.as_str())
    });
    let call_id = events.iter().find_map(|event| {
        event
            .get("item")
            .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("function_call"))
            .and_then(|item| item.get("call_id"))
            .and_then(|id| id.as_str())
    });
    let arguments = events.iter().find_map(|event| {
        if event.get("type").and_then(|value| value.as_str())
            == Some("response.function_call_arguments.done")
        {
            event.get("arguments").and_then(|value| value.as_str())
        } else {
            event
                .get("item")
                .and_then(|item| item.get("arguments"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
        }
    });
    let (response_id, call_id, arguments) = match (response_id, call_id, arguments) {
        (Some(response_id), Some(call_id), Some(arguments)) => {
            (response_id, call_id, arguments)
        }
        _ => return Err("Responses stream did not produce a tool call".into()),
    };
    let sentinel_is_valid = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| value.get("value").and_then(|value| value.as_str()).map(str::to_owned))
        .is_some_and(|value| value == "ready");
    if !sentinel_is_valid {
        return Err("Responses tool call did not return the sentinel arguments".into());
    }

    let second = client
        .post(&url)
        .bearer_auth(credential)
        .json(&serde_json::json!({
            "model": descriptor.model_id,
            "previous_response_id": response_id,
            "input": [{
                "type": "function_call_output",
                "call_id": call_id,
                "output": "verified"
            }],
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
        return Err("Responses stream did not produce text or reasoning after the tool result".into());
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
    let descriptor = preferences::endpoint_model_descriptor(&pairing);
    let compatibility = preferences::pairing_compatibility(&pairing);
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
        record.status = incompatible_status(&pairing);
        record.reason = compatibility.reason;
        return save_result(record);
    }
    let credential = account
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| "provider credential is missing".to_string())?;
    let install = codex::discover_supported_install(env_type)?;
    record.pairing_signature = signature_for(&pairing, &account, &install);
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
        runtime = install.runtime_identity,
        endpoint_host,
        model_id = descriptor.model_id,
        status = ?record.status,
        duration_ms = started.elapsed().as_millis(),
        "provider pairing verification completed"
    );
    save_result(record)
}

pub fn matching_verification(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    install: &codex::CodexInstall,
) -> Option<PairingVerification> {
    let expected = signature_for(pairing, account, install);
    let mut record = preferences::load()
        .ok()?
        .pairing_verifications
        .into_iter()
        .find(|record| {
            record.harness_id == pairing.harness_id
                && record.provider_id == pairing.provider_id
                && record.runtime == install.runtime_identity
        })?;
    if record.pairing_signature != expected || !record.capability_result.compatible {
        record.status = PairingVerificationStatus::Stale;
        record.reason = Some("routing inputs changed; verify the pairing again".into());
    }
    Some(record)
}

pub struct VerifiedCodexPairing {
    pub descriptor: crate::agent::provider::compatibility::EndpointModelDescriptor,
    pub credential: String,
    pub verification: PairingVerification,
    pub install: codex::CodexInstall,
}

/// Single authority for Codex launch eligibility. Spawn-option generation
/// projects this result to a boolean; final preflight preserves its reason.
pub fn verified_codex_pairing(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    env_type: EnvType,
) -> Result<VerifiedCodexPairing, String> {
    // Reject declared protocol/model incompatibility before touching the
    // filesystem, invoking Codex, or considering the endpoint.
    let compatibility = preferences::pairing_compatibility(pairing);
    if !compatibility.compatible {
        return Err(format!(
            "pairing '{}' is unsupported: {}",
            account.name,
            compatibility
                .reason
                .unwrap_or_else(|| "incompatible capability contract".into())
        ));
    }
    let install = codex::discover_supported_install(env_type)?;
    verified_codex_pairing_with_install(pairing, account, &install)
}

fn verified_codex_pairing_with_install(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    install: &codex::CodexInstall,
) -> Result<VerifiedCodexPairing, String> {
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
    let compatibility = preferences::pairing_compatibility(pairing);
    if !compatibility.compatible {
        return Err(format!(
            "pairing '{}' is unsupported: {}",
            account.name,
            compatibility
                .reason
                .unwrap_or_else(|| "incompatible capability contract".into())
        ));
    }
    let verification = matching_verification(pairing, account, install).ok_or_else(|| {
        format!(
            "pairing '{}' is unverified for {}; use Verify pairing in Settings",
            account.name,
            install.runtime_identity
        )
    })?;
    if verification.status != PairingVerificationStatus::Verified {
        return Err(format!(
            "pairing '{}' is not launchable: {}",
            account.name,
            verification
                .reason
                .unwrap_or_else(|| format!("verification status is {:?}", verification.status))
        ));
    }
    Ok(VerifiedCodexPairing {
        descriptor,
        credential: credential.to_string(),
        verification,
        install: install.clone(),
    })
}

pub fn launchable_on_runtime(
    pairing: &ProviderPairing,
    account: &ProviderAccount,
    _env_type: EnvType,
    codex_install: Option<&codex::CodexInstall>,
) -> bool {
    if pairing.surface == preferences::ApiSurface::Anthropic {
        return account.enabled
            && account
                .api_key
                .as_deref()
                .is_some_and(|credential| !credential.trim().is_empty())
            && preferences::pairing_compatibility(pairing).compatible;
    }
    codex_install.is_some_and(|install| {
        verified_codex_pairing_with_install(pairing, account, install).is_ok()
    })
}

pub fn current_statuses(env_type: EnvType) -> Vec<PairingVerification> {
    let prefs = preferences::load().unwrap_or_default();
    let accounts = preferences::provider_accounts();
    let install = codex::discover_supported_install(env_type);
    prefs
        .provider_pairings
        .iter()
        .filter_map(|pairing| {
            let account = accounts.iter().find(|account| account.id == pairing.provider_id)?;
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
                Ok(install) => matching_verification(pairing, account, install)
                    .or_else(|| Some(base())),
                Err(reason) => {
                    let mut record = base();
                    record.status = PairingVerificationStatus::Failed;
                    record.reason = Some(reason.clone());
                    Some(record)
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::compatibility::{
        complete_agent_capabilities, EndpointModelDescriptor, ProviderAuthMode, WireApi,
    };
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            bytes.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&bytes);
            let Some(header_end) = text.find("\r\n\r\n") else {
                continue;
            };
            let content_length = text[..header_end]
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length: ")
                        .or_else(|| line.strip_prefix("Content-Length: "))
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }

    fn respond(stream: &mut TcpStream, status: &str, body: &str) {
        // Lenient: a parallel test runner can schedule the client and
        // server threads such that the client (HTTP layer) closes the
        // connection *between* the test-server's chunked writes — the
        // next write then surfaces `ConnectionReset (10054)` and the
        // `unwrap()` panic propagates back through `server.join()`.
        // The test-server is best-effort: once the client is gone,
        // there's nothing useful to send, so bail instead of panicking.
        // The test contract (client observes the status, error doesn't
        // echo the credential) is preserved because the client has
        // already read enough by the time it closes.
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        if stream.flush().is_err() {
            return;
        }
        for event in body.split_inclusive("\n\n") {
            if write!(stream, "{:x}\r\n{event}\r\n", event.len()).is_err() {
                return;
            }
            if stream.flush().is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = write!(stream, "0\r\n\r\n");
        let _ = stream.flush();
    }

    fn descriptor(endpoint: String) -> EndpointModelDescriptor {
        EndpointModelDescriptor {
            provider_id: "minimax".into(),
            endpoint,
            wire_api: WireApi::Responses,
            model_id: "MiniMax-M3".into(),
            capabilities: complete_agent_capabilities(),
            auth_modes: vec![ProviderAuthMode::BearerEnv],
            context_window: None,
            reasoning_effort: None,
        }
    }

    #[test]
    fn fake_responses_server_proves_streaming_tool_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for step in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert!(request.starts_with("POST /responses HTTP/1.1"), "{request}");
                assert!(request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer sentinel-key"));
                assert!(request.contains("\"model\":\"MiniMax-M3\""));
                if step == 0 {
                    assert!(request.contains("buildmesh_verification"));
                    respond(
                        &mut stream,
                        "200 OK",
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\"}}\n\ndata: {\"type\":\"response.function_call_arguments.done\",\"arguments\":\"{\\\"value\\\":\\\"ready\\\"}\"}\n\n",
                    );
                } else {
                    assert!(request.contains("function_call_output"));
                    assert!(request.contains("call_1"));
                    respond(
                        &mut stream,
                        "200 OK",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"verified\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\"}}\n\n",
                    );
                }
            }
        });

        verify_responses_agent_loop(&descriptor(endpoint), "sentinel-key").unwrap();
        server.join().unwrap();
    }

    #[test]
    fn verification_errors_never_echo_the_credential() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&mut stream);
            respond(&mut stream, "401 Unauthorized", "denied");
        });
        let secret = "sentinel-secret-never-log";
        let error = verify_responses_agent_loop(&descriptor(endpoint), secret).unwrap_err();
        assert!(!error.contains(secret));
        assert!(error.contains("authentication failed"));
        server.join().unwrap();
    }

    #[test]
    fn incompatible_pairing_fails_before_the_trap_endpoint_or_codex_discovery() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let pairing = ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "kimi".into(),
            surface: preferences::ApiSurface::OpenAI,
            base_url: Some(format!("https://{}", listener.local_addr().unwrap())),
            model_tiers: preferences::ModelTiers {
                default: Some("kimi-k3".into()),
                ..Default::default()
            },
        };
        let account = ProviderAccount {
            id: "kimi".into(),
            name: "Kimi".into(),
            enabled: true,
            billing_mode: preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("trap-secret".into()),
        };

        let error = verified_codex_pairing(&pairing, &account, EnvType::Windows)
            .err()
            .expect("Chat Completions pairing must fail closed");
        assert!(error.contains("unsupported"), "{error}");
        assert_eq!(incompatible_status(&pairing), PairingVerificationStatus::Unsupported);
        assert!(matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    }

    #[test]
    fn retired_minimax_alias_is_a_repairable_failure_not_unsupported() {
        let pairing = ProviderPairing {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            surface: preferences::ApiSurface::OpenAI,
            base_url: Some("https://api.minimax.io/v1".into()),
            model_tiers: preferences::ModelTiers {
                default: Some("MiniMax-M3[1m]".into()),
                ..Default::default()
            },
        };
        assert_eq!(incompatible_status(&pairing), PairingVerificationStatus::Failed);
        assert!(
            preferences::pairing_compatibility(&pairing)
                .reason
                .unwrap()
                .contains("MiniMax-M3")
        );
    }
}
