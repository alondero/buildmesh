// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::preferences::BillingMode;

fn account(id: &str, api_key: Option<&str>) -> ProviderAccount {
    ProviderAccount {
        id: id.to_string(),
        name: id.to_string(),
        enabled: true,
        billing_mode: BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: api_key.map(str::to_string),
    }
}

#[test]
fn api_key_rejects_missing_and_empty_keys() {
    assert_eq!(api_key_for(&[], "keyed-test"), None);
    assert_eq!(
        api_key_for(&[account("keyed-test", None)], "keyed-test"),
        None
    );
    assert_eq!(
        api_key_for(&[account("keyed-test", Some(""))], "keyed-test"),
        None
    );
    assert_eq!(
        api_key_for(&[account("keyed-test", Some("k"))], "keyed-test"),
        Some("k")
    );
    // Wrong provider id never leaks a key across providers.
    assert_eq!(
        api_key_for(&[account("other", Some("k"))], "keyed-test"),
        None
    );
}

#[test]
fn shared_client_applies_a_configured_request_timeout() {
    // Verify the configured timeout on the actual production fetch
    // path: install a 1s client via `with_client_override` (the
    // shared_client seam), drive the production `fetch_usage`
    // against an idle loopback listener, and assert the timeout
    // envelope AND a narrow band tied to the configured value
    // (0.8s..1.8s). The previous draft reproduced the
    // request/status/parse algorithm in `fetch_usage_with`; this
    // test exercises the production code itself.
    //
    // Tying the upper bound to the configured value with a small
    // tolerance catches the regression class where the timeout
    // is reduced (say) to 5s without anybody noticing because the
    // test still passes.
    //
    // Issue #1745: `fetch_usage` now returns `UsageOutcome`; the
    // timeout path is `UsageOutcome::Unavailable { reason: "Request failed: ..." }`.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
    let client = blocking_client_builder()
        .timeout(TIMEOUT)
        .build()
        .expect("test client build");
    let (port, _server) = spawn_idle_loopback();
    let start = std::time::Instant::now();
    let outcome = with_client_override(client, || {
        fetch_usage(
            ResponsePolicy::default(),
            |c| c.get(format!("http://127.0.0.1:{port}/usage")),
            |body| {
                Ok(UsageReading {
                    windows: vec![],
                    balance: None,
                    meters: vec![],
                    detail: Some(body.to_string()),
                })
            },
        )
    });
    let elapsed = start.elapsed();
    let reason = match &outcome {
        UsageOutcome::Unavailable { reason } => reason.as_str(),
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    };
    assert!(
        reason.starts_with("Request failed:"),
        "expected reqwest timeout envelope, got reason: {reason:?}"
    );
    // IdleServer Drop joins the worker thread on every exit path
    // (success or panic); the assertion below proves the timeout
    // fired AFTER the connection was accepted, not because the
    // listener was closed prematurely.
    assert!(
        elapsed >= std::time::Duration::from_millis(800),
        "1s timeout must wait for the configured value; elapsed {elapsed:?}"
    );
    assert!(
        elapsed < TIMEOUT + std::time::Duration::from_millis(800),
        "configured timeout is {TIMEOUT:?}; elapsed {elapsed:?} should be within 800ms"
    );
}

/// Spawns a TCP listener on `127.0.0.1:0` and a worker thread that
/// accepts one connection then idles until the returned [`IdleServer`]
/// guard's `Drop` runs. The guard deterministically stops the worker
/// thread and joins it so a panic in the test body still cleans up.
fn spawn_idle_loopback() -> (u16, IdleServer) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(false).expect("blocking accept");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_for_thread = std::sync::Arc::clone(&stop);
    let listener_thread = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            while !stop_for_thread.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            drop(stream);
        }
    });
    let server = IdleServer {
        stop,
        handle: Some(listener_thread),
    };
    (port, server)
}

/// RAII guard: flips `stop` and joins the worker thread on Drop, so a
/// panic in the test body still cleans up the listener.
struct IdleServer {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for IdleServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
