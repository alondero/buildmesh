//! Per-caller rate cap on `POST /api/ws-ticket` (issue #552).
//!
//! `POST /api/ws-ticket` mints an in-memory single-use WebSocket handshake
//! ticket ([`super::ws_ticket`]) that gates every subsequent WS upgrade. Each
//! successful mint costs an in-memory allocation and creates a short-lived
//! reservation the upgrade consumes — a sustained flooder on a valid token
//! can churn the table and starve legitimate clients of a free ticket slot.
//! This module bounds the flood by counting mints per token over a rolling
//! window: a default of [`DEFAULT_MAX_PER_WINDOW`] mints per [`WINDOW`]
//! per token keeps reconnect storms of one well-behaved phone under the cap
//! while making any sustained flood observable.
//!
//! # Why per-token, not per-IP
//!
//! The credential is a high-entropy token, the IP is something a phone's
//! NAT/VPN reshuffles on every reconnect. Capping per IP would penalise a
//! legitimate phone that roams between Wi-Fi and cellular; per-token
//! matches the *thing a leaked credential could abuse* (the same token from
//! the same place or another) and gives revocation a free pass (deleting a
//! device row stops the next auth, the rate-limit counter ages out the
//! rest).
//!
//! # Why in-memory, not SQLite
//!
//! The rate-limit decision is on the hot path of every WS reconnect — a
//! SQLite write per mint would be wasted IO and would compound the very
//! DoS class this module exists to bound. The state is intentionally
//! process-local: a process restart is the documented way to clear it, the
//! same way [`super::ws_ticket::TICKETS`] is wiped on restart.
//!
//! # Sliding window
//!
//! A fixed minute-boundary bucket would let a client burn
//! `DEFAULT_MAX_PER_WINDOW` in the last second of one minute and the same
//! again in the first second of the next — doubling the effective cap. A
//! rolling 60-second window is the natural "30 per any 60 seconds" the issue
//! asks for, and it costs the same per-mint work (a `Vec<Instant>` of recent
//! timestamps, pruned on read).

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

/// How long a token's mint timestamps are remembered. Sized so a phone that
/// reconnects once per minute and re-mints once for each reconnect never
/// hits the cap under normal operation, but a script firing 60×/sec is
/// immediately visible.
pub const WINDOW: Duration = Duration::from_secs(60);

/// The default mint cap per [`WINDOW`] per token. Sized for a healthy
/// phone's "reconnect storm of 1" (issue wording: "e.g. ~30/minute/token,
/// to be tuned"). Exposed as a `const` so a future knob can plumb through
/// a config without rewriting the call sites.
pub const DEFAULT_MAX_PER_WINDOW: usize = 30;

/// Decision returned from a rate-limit check.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The caller is under the cap — proceed to mint.
    Allow,
    /// The caller has exhausted the cap. `retry_after` is a
    /// `Retry-After`-shaped hint in seconds (`>= 1`) — the soonest the
    /// oldest mint in the window will fall out, freeing a slot.
    Deny { retry_after: u32 },
}

type Counters = HashMap<String, Vec<Instant>>;

static COUNTERS: OnceLock<RwLock<Counters>> = OnceLock::new();

fn store() -> &'static RwLock<Counters> {
    COUNTERS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Check whether `fingerprint` may mint another ticket, recording the mint
/// timestamp on the Allow path so the next call sees it. The fingerprint is
/// the SHA-256 of the bearer/cookie credential (callers in
/// `super::mod::handle_connection` derive it via [`crate::db::hash_token`])
/// — raw tokens never become map keys, so a process dump of this module's
/// state doesn't leak a credential.
///
/// `max_per_window` is taken as a parameter so unit tests can drive a tiny
/// window/cap without time-warping the production constants.
pub fn check_and_record(fingerprint: &str, now: Instant, max_per_window: usize) -> Outcome {
    debug_assert!(max_per_window > 0, "cap must be positive");
    let window = WINDOW;
    let mut counters = store().write();
    let timestamps = counters.entry(fingerprint.to_string()).or_default();
    // Prune anything older than the window. Doing this on every check keeps
    // the worst-case per-mint work bounded by the cap (the oldest entries
    // fall out steadily; we never scan the whole window's worth).
    timestamps.retain(|t| now.duration_since(*t) < window);
    if timestamps.len() >= max_per_window {
        // The caller is at the cap. The soonest slot frees when the OLDEST
        // timestamp ages out — `Retry-After` is computed from that, with a
        // 1-second floor so a sub-second remainder doesn't round down to
        // zero (which would defeat the header's purpose).
        let oldest = *timestamps.iter().min().expect("len >= max >= 1");
        let elapsed = now.duration_since(oldest);
        let remaining = window.saturating_sub(elapsed);
        // Round up; 1-second floor; saturating cast to u32 — minutes+
        // overflow is a wall-clock bug we'd want to know about anyway.
        let secs = remaining.as_secs();
        let frac = if remaining.subsec_millis() > 0 { 1 } else { 0 };
        let retry = secs.saturating_add(frac).max(1).min(u32::MAX as u64) as u32;
        Outcome::Deny { retry_after: retry }
    } else {
        timestamps.push(now);
        Outcome::Allow
    }
}

/// Test seam: drain all per-fingerprint state so a unit test can run against
/// the production constants without bleeding counters into a sibling test.
/// `cfg(test)` so it never lands in the production binary.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    store().write().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize tests that touch the process-global rate-limit state. The
    /// counter store is a `OnceLock<RwLock<HashMap<…>>>` (mirroring
    /// `super::ws_ticket::TICKETS`), so concurrent tests using the same
    /// fingerprint race each other's writes. Each test uses a unique
    /// fingerprint prefix *and* takes this lock as defence-in-depth so a
    /// future test that forgets the prefix doesn't silently corrupt the
    /// state the next test expects.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn fp(s: &str) -> String {
        crate::db::hash_token(s)
    }

    #[test]
    fn under_cap_returns_allow() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        let now = Instant::now();
        // A cap of 5 keeps the test cheap.
        for i in 0..5u64 {
            assert_eq!(
                check_and_record(&fp("under-cap:alice"), now + Duration::from_secs(i), 5),
                Outcome::Allow,
                "mint {i} must be Allowed under the cap"
            );
        }
    }

    #[test]
    fn at_cap_returns_deny_with_retry_after() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        let now = Instant::now();
        // Fill the cap exactly.
        for i in 0..5u64 {
            assert_eq!(
                check_and_record(&fp("at-cap:alice"), now + Duration::from_secs(i), 5),
                Outcome::Allow,
            );
        }
        // The next request is the (cap+1)-th in the window → Deny.
        match check_and_record(&fp("at-cap:alice"), now + Duration::from_secs(5), 5) {
            Outcome::Deny { retry_after } => {
                // Oldest entry was minted at `now`; request time is `now+5s`;
                // window is 60s → 55s of window remain → retry_after ≈ 55.
                assert!(
                    (50..=60).contains(&retry_after),
                    "retry_after should be ~55s for a 60s window; got {retry_after}"
                );
            }
            Outcome::Allow => panic!("over-cap request was unexpectedly Allowed"),
        }
    }

    #[test]
    fn per_token_isolation() {
        let _g = TEST_LOCK.lock().unwrap();
        // AC: "per-token isolation (one token's flood does not affect another)".
        reset_for_test();
        let now = Instant::now();
        // alice floods her cap.
        for _ in 0..3usize {
            let _ = check_and_record(&fp("iso:alice"), now, 3);
        }
        match check_and_record(&fp("iso:alice"), now, 3) {
            Outcome::Deny { .. } => {}
            Outcome::Allow => panic!("alice must be over her cap"),
        }
        // bob — distinct fingerprint — still gets a free slot.
        assert_eq!(
            check_and_record(&fp("iso:bob"), now, 3),
            Outcome::Allow,
            "bob's bucket must be independent of alice's"
        );
    }

    #[test]
    fn window_resets_after_the_interval() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        let cap: usize = 3;
        let window = WINDOW;
        let t0 = Instant::now();
        // Burn the cap at t0.
        for i in 0..cap {
            assert_eq!(
                check_and_record(&fp("window:alice"), t0 + Duration::from_millis(i as u64), cap),
                Outcome::Allow
            );
        }
        // Still at cap immediately after.
        assert!(matches!(
            check_and_record(&fp("window:alice"), t0 + Duration::from_secs(1), cap),
            Outcome::Deny { .. }
        ));
        // After the window has elapsed, every timestamp has aged out — a new
        // mint is Allowed. Plus a hair so the equality `>` vs `>=` doesn't
        // introduce a flake at the exact-window boundary.
        let past = t0 + window + Duration::from_millis(10);
        assert_eq!(
            check_and_record(&fp("window:alice"), past, cap),
            Outcome::Allow,
            "after the window elapses, the bucket is fully drained"
        );
    }

    #[test]
    fn retry_after_is_at_least_one_second() {
        let _g = TEST_LOCK.lock().unwrap();
        // Even when the oldest entry is a hair short of expiring, the header
        // MUST be a positive integer — a `Retry-After: 0` would invite
        // instant retry loops.
        reset_for_test();
        let t0 = Instant::now();
        let _ = check_and_record(&fp("floor:alice"), t0, 1);
        let probe = t0 + WINDOW - Duration::from_millis(10);
        match check_and_record(&fp("floor:alice"), probe, 1) {
            Outcome::Deny { retry_after } => {
                assert!(retry_after >= 1, "Retry-After must be >= 1; got {retry_after}");
            }
            Outcome::Allow => panic!("over-cap should be Deny"),
        }
    }

    #[test]
    fn distinct_fingerprints_share_no_state() {
        let _g = TEST_LOCK.lock().unwrap();
        // Even a single-character difference in the fingerprint produces an
        // independent bucket — the same boolean the production path relies on
        // when two phones have two distinct device tokens.
        reset_for_test();
        let now = Instant::now();
        let _ = check_and_record(&fp("phones:phone-A"), now, 1);
        assert_eq!(
            check_and_record(&fp("phones:phone-B"), now, 1),
            Outcome::Allow,
            "different fingerprints must have independent counters"
        );
    }

    #[test]
    fn default_constants_are_documented_for_reconnect_storms() {
        // Pin the public default — a refactor that drops the cap accidentally
        // is the kind of regression a hard constant breaks loudly.
        assert_eq!(DEFAULT_MAX_PER_WINDOW, 30, "issue #552 default is 30/minute/token");
        assert_eq!(WINDOW, Duration::from_secs(60), "issue #552 window is 60s");
    }
}
