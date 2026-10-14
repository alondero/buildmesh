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
//!
//! # Bounded working set (issue #1233)
//!
//! The bucket count itself is capped at [`MAX_BUCKETS`]. The rate-limit check
//! on `POST /api/ws-ticket` deliberately runs BEFORE the auth guard so that a
//! stolen-token flooder can't distinguish "rate-limited" from "bad token";
//! the same deliberateness makes the bucket map a remote memory-exhaustion
//! vector when a peer sends `Authorization: Bearer <garbage>` per request
//! with distinct random tokens. Without a cap, each unique fingerprint
//! commits one `HashMap` entry to the map forever — a sustained flood
//! grows RSS without bound until app restart. [`check_and_record`] both
//! drops now-empty buckets and evicts the longest-dormant buckets when
//! the working set would otherwise exceed `MAX_BUCKETS`, so the map size
//! is bounded independent of how many distinct credentials arrive.

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

/// Upper bound on the number of distinct fingerprint buckets held in memory
/// at any time (issue #1233). Sized to keep the rate-limit state comfortably
/// inside one Process Memory Info page even at saturation
/// (10_000 SHA-256 hex keys × ~80 bytes/entry ≈ ~1 MB), and to keep the
/// per-call eviction scan — a linear pass over the map — bounded at
/// `O(MAX_BUCKETS)`.
///
/// The cap is reached only by a sustained flood of DISTINCT junk credentials:
/// a legitimate caller (one phone = one fingerprint) occupies O(1) buckets.
/// Beyond the cap we evict the buckets whose newest stamp is oldest — a
/// well-behaved caller whose bucket was evicted re-creates it on the next
/// mint at zero cost beyond one HashMap entry, the same cost their first
/// mint already paid.
///
/// Exposed at crate scope (`pub(crate)`) so the bounded-growth regression
/// test can pin "live ≤ MAX_BUCKETS" against a named constant rather than
/// the magic number.
pub(crate) const MAX_BUCKETS: usize = 10_000;

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
    let outcome = if timestamps.len() >= max_per_window {
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
        // Record FIRST so the Allow decision sees the stamp it just pushed
        // (the issue #1233 ordering note: "recording on Allow must still
        // push before deciding"). The post-decision hygiene below never sees
        // an empty Vec on this branch — push guarantees `len() >= 1`.
        timestamps.push(now);
        Outcome::Allow
    };

    // Bounded-memory hygiene (issue #1233).
    //
    // `counters.entry(fp).or_default()` always materialises a HashMap entry,
    // so a flood of distinct junk credentials can grow the map without bound.
    // That matters specifically here because the rate-limit check on
    // `POST /api/ws-ticket` runs BEFORE the auth guard
    // (`http::mod::handle_connection`), by design — so bogus tokens get
    // counted too. Without a hard cap, a LAN-exposed peer sending
    // `Authorization: Bearer <random>` per request could exhaust app
    // memory in minutes. Two complementary guards:
    //
    //   1. Drop empty buckets. A `timestamps` Vec that was pruned to length
    //      zero and we didn't push (only theoretical today because `push`
    //      always runs on Allow and Deny requires `len >= max >= 1`) leaves
    //      an `entry` wrapper with nothing in it. Future reordering that
    //      records AFTER decide or supports `max_per_window == 0` would
    //      expose this branch — the early-drop is a defensive one-liner.
    if timestamps.is_empty() {
        counters.remove(fingerprint);
    }
    //
    //   2. Hard cap on the total number of buckets. When `counters.len()`
    //      exceeds `MAX_BUCKETS` (only reachable via a flood of distinct
    //      credentials — legitimate callers own O(1) buckets), evict the
    //      buckets whose NEWEST stamp is the oldest of the working set.
    //      Eviction policy mirrors a memory-pressure cache: the longest-
    //      dormant bucket is the least likely to be touched again soon,
    //      and re-creating it on the next legitimate mint costs one
    //      HashMap entry — the same cost the first mint already paid.
    //      Linear scan under the write lock is fine at this size; a
    //      well-behaved phone pushes the working set to 1, so the scan
    //      runs over the junk, not over its own entries.
    if counters.len() > MAX_BUCKETS {
        let to_evict = counters.len() - MAX_BUCKETS;
        evict_oldest_newest(&mut counters, to_evict);
    }

    outcome
}

/// Evict up to `n` buckets from `counters`, choosing those whose NEWEST stamp
/// is oldest first. Empty buckets (no stamps remaining after pruning) are
/// preferred — `Vec::last()` on an empty slice yields `None`, which sorts
/// before any `Some(Instant)`, so they're picked automatically.
///
/// Called with the write lock already held by [`check_and_record`] and at
/// most `(counters.len() - MAX_BUCKETS)` iterations of work — O(n) per
/// scan over O(MAX_BUCKETS) entries, well under 1ms in the steady state.
fn evict_oldest_newest(counters: &mut Counters, n: usize) {
    for _ in 0..n {
        if counters.is_empty() {
            break;
        }
        // The clone is unavoidable: `min_by_key` borrows immutably while we
        // need to mutate. Keys are SHA-256 hex strings (~64 bytes), so the
        // allocation per evict is bounded and amortised.
        let victim = counters
            .iter()
            .min_by_key(|(_, stamps)| stamps.last().copied())
            .map(|(k, _)| k.clone());
        let Some(k) = victim else { break };
        counters.remove(&k);
    }
}

/// Test seam: drain all per-fingerprint state so a unit test can run against
/// the production constants without bleeding counters into a sibling test.
/// `cfg(test)` so it never lands in the production binary.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    store().write().clear();
}

/// Test seam: current map size. Used by the bounded-growth regression test
/// (issue #1233) to assert that sustained floods of distinct junk credentials
/// cannot grow the rate-limiter's footprint past `MAX_BUCKETS`. Mirrors
/// `reset_for_test` in shape — same `cfg(test)` gating.
#[cfg(test)]
pub(crate) fn len() -> usize {
    store().read().len()
}

/// Process-wide test mutex shared with the dispatcher-level tests in
/// `http::tests` (which used to carry a parallel `RATE_LIMIT_DISPATCH_LOCK`
/// for the same purpose). Because every rate-limit test — unit OR
/// dispatcher — touches the same global counter, a single mutex is the only
/// correct primitive; two independent locks race each other and let
/// `reset_for_test()` wipe state mid-assertion. Lives at module scope so
/// sibling test modules can `use` it. `cfg(test)` so it never lands in the
/// production binary.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    // `TEST_LOCK` is the module-scoped `pub(crate) static` declared above.
    // Each test takes it as defence-in-depth so a future test that forgets
    // to use a unique fingerprint prefix doesn't silently corrupt the
    // state the next test expects. The same lock is also taken by the
    // dispatcher-level tests in `http::tests`, which used to carry their
    // own `RATE_LIMIT_DISPATCH_LOCK` and race against this one; see the
    // `pub(crate) static TEST_LOCK` decl above for the rationale.

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
                check_and_record(
                    &fp("window:alice"),
                    t0 + Duration::from_millis(i as u64),
                    cap
                ),
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
                assert!(
                    retry_after >= 1,
                    "Retry-After must be >= 1; got {retry_after}"
                );
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
        assert_eq!(
            DEFAULT_MAX_PER_WINDOW, 30,
            "issue #552 default is 30/minute/token"
        );
        assert_eq!(WINDOW, Duration::from_secs(60), "issue #552 window is 60s");
    }

    #[test]
    fn distinct_fingerprints_stay_bounded_under_flood() {
        // Regression for issue #1233: a LAN-exposed peer can drive
        // `POST /api/ws-ticket` with random `Authorization: Bearer <garbage>`
        // headers, and the rate-limit check runs BEFORE the auth guard
        // (`http::mod::handle_connection`). Each unique fingerprint therefore
        // counts as a bucket owner — and the bucket map MUST stay bounded
        // independent of how many distinct credentials arrive. Without the
        // total-entry cap added by the fix, `counters.entry(fp).or_default()`
        // would grow the map by ~one entry per request until the process is
        // restarted.
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();

        // 1.5× the cap, deliberately oversized so any finite-but-not-capped
        // implementation would leak the difference. With cap=1 each call is
        // its own one-stamp bucket and the fingerprint is never reused, so
        // per-bucket pruning (`Vec::retain`) never re-runs against it — the
        // total-entry cap is the only thing that keeps the map bounded.
        let n = super::MAX_BUCKETS + super::MAX_BUCKETS / 2;
        for i in 0..n {
            let finger = format!("flood-credential-{i:08}");
            let _ = check_and_record(&finger, Instant::now(), 1);
        }

        let live = len();
        assert!(
            live <= super::MAX_BUCKETS,
            "rate-limit map grew past MAX_BUCKETS under distinct credentials: \
             live={live}, cap={} (issue #1233)",
            super::MAX_BUCKETS
        );
    }
}
