// Tests for `services::provider_verification`. The owner keeps only the
// sibling module declaration; the bodies live here so the production file
// carries no test code (the repo's rust-test-layout gate).

use super::*;
use crate::agent::provider::compatibility::{
    complete_agent_capabilities, CompatibilityDecision, EndpointModelDescriptor, ProviderAuthMode,
    WireApi,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

type CapturedEventFields = Vec<(String, String)>;
type CapturedEvents = std::sync::Arc<std::sync::Mutex<Vec<CapturedEventFields>>>;

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
                // Codex sends strict function tools — the probe must too.
                assert!(request.contains("\"strict\":true"), "{request}");
                respond(
                    &mut stream,
                    "200 OK",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"buildmesh_verification\",\"arguments\":\"{}\"}}\n\ndata: {\"type\":\"response.function_call_arguments.done\",\"arguments\":\"{\\\"value\\\":\\\"ready\\\"}\"}\n\n",
                );
            } else {
                // MiniMax's Responses API has no `previous_response_id`
                // and returns HTTP 500 for it — the tool result must be
                // replayed as explicit input items (the documented
                // multi-turn pattern, also valid OpenAI Responses).
                assert!(
                    !request
                        .to_ascii_lowercase()
                        .contains("previous_response_id"),
                    "{request}"
                );
                assert!(request.contains("function_call_output"));
                assert!(request.contains("call_1"));
                assert!(
                    request.contains("\"name\":\"buildmesh_verification\""),
                    "{request}"
                );
                assert!(request.contains("\"type\":\"function_call\""), "{request}");
                assert!(request.contains("\"type\":\"message\""), "{request}");
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

/// Issue #1386: per-test-thread storage means the `APP_DATA_DIR`/`CACHE`
/// globals are owned by THIS `cargo test` worker thread for the
/// duration of each test, so we no longer need either this module's
/// `PREFS_TEST_LOCK` (paired-contention with peer preferences tests) or
/// `preferences::test_state_guard()` (process-global contention). Each
/// test gets its own `init_for_tests` + `reset_for_tests` round and
/// collateral tests run concurrently without seeing each other's
/// state.
/// A verification recorded against an older Codex CLI (npm auto-updates)
/// must report the CLI change as the staleness cause — not the generic
/// "routing inputs changed" message that masks the real state.
#[test]
fn stale_after_cli_update_reports_the_cli_change_not_routing() {
    // `tempfile::tempdir()` returns a unique directory per call AND
    // cleans up on `Drop` — much safer than the previous
    // `std::env::temp_dir().join(format!("buildmesh-pv-test-{pid}"))`,
    // which collided across parallel tests running under the same
    // process ID (round-4 review). The `TempDir` binding must stay
    // alive until the test ends, so we hold it until the explicit
    // remove in the cleanup epilogue.
    let tmp = tempfile::tempdir().expect("tempdir");
    preferences::init_for_tests(tmp.path().to_path_buf());

    let pairing = ProviderPairing {
        harness_id: "codex".into(),
        provider_id: "minimax".into(),
        surface: preferences::ApiSurface::OpenAI,
        base_url: Some("https://api.minimax.io/v1".into()),
        model_tiers: preferences::ModelTiers {
            default: Some("MiniMax-M3".into()),
            ..Default::default()
        },
    };
    let account = ProviderAccount {
        id: "minimax".into(),
        name: "MiniMax".into(),
        enabled: true,
        billing_mode: preferences::BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: Some("sentinel-key".into()),
    };
    let old_install = codex::CodexInstall {
        executable: r"C:\npm\codex.cmd".into(),
        version: "0.148.0".into(),
        runtime_identity: "native-windows".into(),
        codex_home: r"C:\Users\u\.codex".into(),
        wsl_distro: None,
    };
    let record = PairingVerification {
        harness_id: pairing.harness_id.clone(),
        provider_id: pairing.provider_id.clone(),
        pairing_signature: signature_for(&pairing, &account, &old_install),
        endpoint: "https://api.minimax.io/v1".into(),
        model_id: "MiniMax-M3".into(),
        auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
        runtime: old_install.runtime_identity.clone(),
        executable: old_install.executable.clone(),
        codex_version: old_install.version.clone(),
        capability_result: CompatibilityDecision {
            compatible: true,
            reason: None,
        },
        status: PairingVerificationStatus::Verified,
        verified_at: Some(Utc::now()),
        reason: None,
    };
    save_result(record.clone()).unwrap();
    let mut other_model = pairing.clone();
    other_model.model_tiers.default = Some("another-model".into());
    let mut other_record = record.clone();
    other_record.model_id = "another-model".into();
    other_record.pairing_signature = signature_for(&other_model, &account, &old_install);
    save_result(other_record).unwrap();
    assert_eq!(preferences::load().unwrap().pairing_verifications.len(), 2);
    assert_eq!(
        matching_verification(&pairing, &account, &old_install)
            .unwrap()
            .status,
        PairingVerificationStatus::Verified
    );
    assert_eq!(
        matching_verification(&other_model, &account, &old_install)
            .unwrap()
            .status,
        PairingVerificationStatus::Verified
    );

    let new_install = codex::CodexInstall {
        version: "0.149.0".into(),
        ..old_install.clone()
    };
    let got = matching_verification(&pairing, &account, &new_install).unwrap();
    assert_eq!(got.status, PairingVerificationStatus::Stale);
    let reason = got.reason.unwrap();
    assert!(
        reason.contains("Codex"),
        "reason should name the CLI change: {reason}"
    );
    assert!(
        !reason.contains("routing inputs"),
        "reason must not blame routing: {reason}"
    );

    // A changed endpoint/model is the genuine routing-change case and must
    // keep the original message.
    let rerouted_pairing = ProviderPairing {
        base_url: Some("https://api.minimax.io/v2".into()),
        ..pairing.clone()
    };
    let got = matching_verification(&rerouted_pairing, &account, &old_install).unwrap();
    assert_eq!(got.status, PairingVerificationStatus::Stale);
    assert_eq!(
        got.reason.as_deref(),
        Some("routing inputs changed; verify the pairing again")
    );

    preferences::reset_for_tests();
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A saved record whose capability snapshot is incompatible (e.g. a WSL
/// leftover verified against the retired `MiniMax-M3[1m]` alias) keeps its
/// real failure status and reason instead of being re-labelled as a stale
/// signature.
#[test]
fn incompatible_capability_record_keeps_its_reason_instead_of_stale_mask() {
    // Issue #1386: no `PREFS_TEST_LOCK` / `test_state_guard` needed —
    // per-thread storage isolates this test from any concurrent sibling.
    // Round-4 review: switch to `tempfile::tempdir()` (cleanup on
    // Drop, unique path per call) instead of the hand-rolled path
    // that collided across parallel tests under the same `pid`.
    let tmp = tempfile::tempdir().expect("tempdir");
    preferences::init_for_tests(tmp.path().to_path_buf());

    let pairing = ProviderPairing {
        harness_id: "codex".into(),
        provider_id: "minimax".into(),
        surface: preferences::ApiSurface::OpenAI,
        base_url: Some("https://api.minimax.io/v1".into()),
        model_tiers: preferences::ModelTiers {
            default: Some("MiniMax-M3".into()),
            ..Default::default()
        },
    };
    let account = ProviderAccount {
        id: "minimax".into(),
        name: "MiniMax".into(),
        enabled: true,
        billing_mode: preferences::BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: Some("sentinel-key".into()),
    };
    let install = codex::CodexInstall {
        executable: String::new(),
        version: String::new(),
        runtime_identity: "wsl".into(),
        codex_home: "/home/u/.codex".into(),
        wsl_distro: None,
    };
    let incompatible_reason =
        "MiniMax Codex pairings require the current Responses model ID 'MiniMax-M3'";
    let record = PairingVerification {
        harness_id: pairing.harness_id.clone(),
        provider_id: pairing.provider_id.clone(),
        pairing_signature: String::new(),
        endpoint: "https://api.minimax.io/v1".into(),
        model_id: "MiniMax-M3[1m]".into(),
        auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
        runtime: install.runtime_identity.clone(),
        executable: String::new(),
        codex_version: String::new(),
        capability_result: CompatibilityDecision {
            compatible: false,
            reason: Some(incompatible_reason.into()),
        },
        status: PairingVerificationStatus::Failed,
        verified_at: None,
        reason: Some(incompatible_reason.into()),
    };
    preferences::update(|prefs| prefs.pairing_verifications.push(record)).unwrap();

    let got = matching_verification(&pairing, &account, &install).unwrap();
    assert_eq!(got.status, PairingVerificationStatus::Failed);
    assert_eq!(got.reason.as_deref(), Some(incompatible_reason));

    preferences::reset_for_tests();
    let _ = std::fs::remove_dir_all(&tmp);
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

    let error = codex_proxy_route(&pairing, &account, EnvType::Windows)
        .err()
        .expect("Chat Completions pairing must fail closed");
    assert!(error.contains("unsupported"), "{error}");
    assert_eq!(
        incompatible_status(&pairing),
        PairingVerificationStatus::Unsupported
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
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
    assert_eq!(
        incompatible_status(&pairing),
        PairingVerificationStatus::Failed
    );
    assert!(preferences::pairing_compatibility(&pairing)
        .reason
        .unwrap()
        .contains("MiniMax-M3"));
}

/// Issue #1937: each `current_statuses` call (twice per Settings →
/// Providers load, once per runtime) must emit exactly one greppable
/// `info` line with its Codex probe cost. The field allow-list pins the
/// redaction discipline: only the runtime identity and CLI version
/// travel, never credentials, keys, or endpoint URLs.
///
/// A hand-rolled capturing subscriber (scoped via `with_default`, so no
/// global state) keeps this parallel-safe without new dev-dependencies.
#[test]
fn current_statuses_emits_one_timing_log_line() {
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata};

    struct Recorder {
        fields: Vec<(String, String)>,
    }
    impl Visit for Recorder {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            // `record_debug` is the `Visit` funnel: every typed
            // `record_*` defaults through it, so one arm captures all
            // fields. String values arrive Debug-quoted — strip the
            // quotes for assertion readability.
            let rendered = format!("{value:?}");
            let unquoted = rendered
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(&rendered)
                .to_string();
            self.fields.push((field.name().to_string(), unquoted));
        }
    }
    struct Capture {
        events: CapturedEvents,
    }
    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut recorder = Recorder { fields: Vec::new() };
            event.record(&mut recorder);
            self.events.lock().unwrap().push(recorder.fields);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    let events: CapturedEvents = Arc::new(Mutex::new(Vec::new()));
    // `tracing` caches per-callsite interest globally: a first hit under
    // the default no-op dispatcher pins the site as disabled, and a
    // rebuild only reaches already-registered sites — so a sibling test
    // hitting `current_statuses` first (or between rebuild and emit)
    // would silently swallow the line under parallelism. Drive one
    // throwaway call first (registering the site under the capturing
    // dispatcher), rebuild, discard its line, and assert on the second
    // call. Whatever the initial cache state, the second call
    // contributes exactly one line — which also proves the "one line per
    // derivation" budget.
    let run_once = || {
        tracing::dispatcher::with_default(
            &tracing::dispatcher::Dispatch::new(Capture {
                events: events.clone(),
            }),
            || {
                tracing::callsite::rebuild_interest_cache();
                current_statuses(EnvType::Windows)
            },
        )
    };
    run_once();
    tracing::callsite::rebuild_interest_cache();
    events.lock().unwrap().clear();
    let statuses = run_once();

    let events = events.lock().unwrap();
    let derivations: Vec<_> = events
        .iter()
        .filter(|fields| {
            fields.iter().any(|(name, value)| {
                name == "message" && value == "pairing verification statuses completed"
            })
        })
        .collect();
    assert_eq!(
        derivations.len(),
        1,
        "expected exactly one pairing verification statuses log line, got {}",
        derivations.len()
    );
    let fields = derivations[0];
    let mut names: Vec<&str> = fields
        .iter()
        .filter(|(name, _)| name != "message")
        .map(|(name, _)| name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "codex_version",
            "pairing_count",
            "probe_duration_ms",
            "runtime",
            "total_duration_ms",
        ],
        "the statuses log field set is the redaction contract — adding a field is a deliberate change"
    );
    let value = |name: &str| {
        fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("statuses log must carry `{name}`"))
    };
    // The timings must flow from the real call, not hardcode a shape:
    // both durations parse as millisecond counts and the pairing count
    // matches the returned statuses.
    value("probe_duration_ms")
        .parse::<u128>()
        .expect("probe_duration_ms must be a millisecond count");
    value("total_duration_ms")
        .parse::<u128>()
        .expect("total_duration_ms must be a millisecond count");
    assert_eq!(
        value("pairing_count")
            .parse::<usize>()
            .expect("pairing_count must parse"),
        statuses.len(),
        "logged pairing_count must match the returned statuses"
    );
    assert!(!value("runtime").is_empty());
    assert!(!value("codex_version").is_empty());
}

// -----------------------------------------------------------------------
// The advisory policy: the pairing verification warns, it does not gate.
// -----------------------------------------------------------------------

fn advisory_pairing() -> ProviderPairing {
    ProviderPairing {
        harness_id: "codex".into(),
        provider_id: "minimax".into(),
        surface: preferences::ApiSurface::OpenAI,
        base_url: Some("https://api.minimax.io/v1".into()),
        model_tiers: preferences::ModelTiers {
            default: Some("MiniMax-M3".into()),
            ..Default::default()
        },
    }
}

fn advisory_account() -> ProviderAccount {
    ProviderAccount {
        id: "minimax".into(),
        name: "MiniMax".into(),
        enabled: true,
        billing_mode: preferences::BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: Some("sentinel-key".into()),
    }
}

fn advisory_install(version: &str) -> codex::CodexInstall {
    codex::CodexInstall {
        executable: r"C:\npm\codex.cmd".into(),
        version: version.into(),
        runtime_identity: "native-windows".into(),
        codex_home: r"C:\Users\u\.codex".into(),
        wsl_distro: None,
    }
}

/// A route that was never checked on this runtime still launches. The
/// check is advisory: the user decides whether the combination works, and
/// the route carries a `Missing` advisory so the spawn can say so.
#[test]
fn an_unchecked_route_launches_with_a_missing_advisory() {
    let pairing = advisory_pairing();
    let account = advisory_account();
    let install = advisory_install("0.166.0");

    let route = codex_proxy_route_with_install(&pairing, &account, &install, &[])
        .expect("an unchecked route must still launch");
    assert_eq!(route.advisory, Some(VerificationAdvisory::Missing));
    assert!(
        route.verification.is_none(),
        "there is no record to carry when the route was never checked"
    );
    assert!(launchable_on_runtime_with(
        &pairing,
        &account,
        EnvType::Windows,
        Some(&install),
        &[]
    ));

    let warning = route
        .advisory
        .as_ref()
        .expect("the advisory carries the warning")
        .warning("MiniMax", "native-windows");
    assert!(warning.contains("MiniMax"), "{warning}");
    assert!(warning.contains("not been checked"), "{warning}");
}

/// A route whose check went stale — a routine `codex-cli` npm update
/// changes the installed version — still launches, and the advisory names
/// the CLI change rather than blaming the route.
#[test]
fn a_stale_route_after_a_cli_update_launches_and_names_the_cli_change() {
    let pairing = advisory_pairing();
    let account = advisory_account();
    let old_install = advisory_install("0.165.0");
    let record = PairingVerification {
        harness_id: pairing.harness_id.clone(),
        provider_id: pairing.provider_id.clone(),
        pairing_signature: signature_for(&pairing, &account, &old_install),
        endpoint: "https://api.minimax.io/v1".into(),
        model_id: "MiniMax-M3".into(),
        auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
        runtime: old_install.runtime_identity.clone(),
        executable: old_install.executable.clone(),
        codex_version: old_install.version.clone(),
        capability_result: CompatibilityDecision {
            compatible: true,
            reason: None,
        },
        status: PairingVerificationStatus::Verified,
        verified_at: Some(Utc::now()),
        reason: None,
    };
    let new_install = advisory_install("0.166.0");

    let route = codex_proxy_route_with_install(
        &pairing,
        &account,
        &new_install,
        std::slice::from_ref(&record),
    )
    .expect("a stale route must still launch");
    let advisory = route.advisory.expect("a stale route carries an advisory");
    let VerificationAdvisory::Stale { reason } = &advisory else {
        panic!("expected a stale advisory, got {advisory:?}");
    };
    assert!(reason.contains("Codex"), "{reason}");
    assert!(reason.contains("0.166.0"), "{reason}");
    assert!(
        !reason.contains("verify the pairing again"),
        "the launch warning already says a re-check is running: {reason}"
    );
    assert!(advisory.wants_recheck());
}

/// A route whose check failed still launches — the user is warned, not
/// blocked — and the advisory keeps the reason the endpoint gave.
#[test]
fn a_route_whose_check_failed_launches_and_keeps_the_failure_reason() {
    let pairing = advisory_pairing();
    let account = advisory_account();
    let install = advisory_install("0.166.0");
    let record = PairingVerification {
        harness_id: pairing.harness_id.clone(),
        provider_id: pairing.provider_id.clone(),
        pairing_signature: signature_for(&pairing, &account, &install),
        endpoint: "https://api.minimax.io/v1".into(),
        model_id: "MiniMax-M3".into(),
        auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
        runtime: install.runtime_identity.clone(),
        executable: install.executable.clone(),
        codex_version: install.version.clone(),
        capability_result: CompatibilityDecision {
            compatible: true,
            reason: None,
        },
        status: PairingVerificationStatus::Failed,
        verified_at: Some(Utc::now()),
        reason: Some("authentication failed (401)".into()),
    };

    let route =
        codex_proxy_route_with_install(&pairing, &account, &install, std::slice::from_ref(&record))
            .expect("a failed route must still launch");
    let advisory = route.advisory.expect("a failed route carries an advisory");
    let VerificationAdvisory::Failed { reason } = &advisory else {
        panic!("expected a failed advisory, got {advisory:?}");
    };
    assert!(reason.contains("authentication failed"), "{reason}");
    assert!(
        !advisory.wants_recheck(),
        "a failed check needs a credential fix, not another probe on every launch"
    );
}

/// The hard failures stay hard: a route the user cannot launch at all
/// still fails closed, with its own reason.
#[test]
fn a_route_without_a_credential_or_with_a_disabled_account_still_fails_closed() {
    let pairing = advisory_pairing();
    let install = advisory_install("0.166.0");

    let mut without_credential = advisory_account();
    without_credential.api_key = None;
    let error = codex_proxy_route_with_install(&pairing, &without_credential, &install, &[])
        .err()
        .expect("a route with no credential must fail closed");
    assert!(error.contains("no credential"), "{error}");

    let mut disabled = advisory_account();
    disabled.enabled = false;
    let error = codex_proxy_route_with_install(&pairing, &disabled, &install, &[])
        .err()
        .expect("a disabled account must fail closed");
    assert!(error.contains("disabled"), "{error}");
}
