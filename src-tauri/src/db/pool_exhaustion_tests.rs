//! Reader-pool exhaustion tests (issue #1533).
//!
//! Pins the contract the production fix closes:
//!
//!   * holding all `READER_POOL_SIZE` leases makes a 9th checkout return
//!     [`DbError::ReaderPoolExhausted`] — *never* panic and *never* abort
//!     the process. Release builds use `panic = "abort"`, so a panic in the
//      checkout path used to terminate the entire application.
//!   * releasing one lease lets the next request succeed (the condvar
//!     notifies a single waiter).
//!   * spurious notifications cannot extend the nominal
//!     `READER_CHECKOUT_TIMEOUT` materially — the absolute-deadline loop in
//!     `ReaderPool::checkout` re-arms the wait with the *remaining* duration
//!     each iteration, so a wake right before the deadline cannot re-arm
//!     the timer. This is exercised by firing `notify_one_for_test` while
//!     all 8 leases are still held (no `Drop`, no real lease freed).
//!   * `current_longest_lease_ms` reports the age of the longest
//!     currently-held lease at the moment of failure — the actionable
//!     "what's hung right now" signal — distinct from
//!     `historical_longest_lease_ms`, the process-lifetime baseline.
//!   * diagnostics carry only bounded numeric fields (`waited_ms`,
//!     `in_use`, `pool_size`, `current_longest_lease_ms`,
//!     `historical_longest_lease_ms`) — no query parameters or secrets.
//!
//! The tests build an isolated [`ReaderPool`] against a `tempfile`-backed
//! SQLite file (so the pragmas in `apply_connection_pragmas` can run as in
//! production) rather than going through the global `DB` `OnceCell` — that
//! keeps the test hermetic and lets us hold every lease without affecting
//! other tests that share the static pool.
//!
//! Run with: `cargo test --package buildmesh --lib db::pool_exhaustion_tests`

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use rusqlite::Connection;

    use super::super::{DbError, ReaderPool, READER_CHECKOUT_TIMEOUT, READER_POOL_SIZE};

    /// Build a minimal on-disk SQLite file with the schema the pool's
    /// `apply_connection_pragmas` expects. Returns the `tempfile::TempDir`
    /// guard so the file lives until the test finishes.
    fn fresh_pool() -> (tempfile::TempDir, ReaderPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("pool_exhaustion.sqlite");
        let conn = Connection::open(&db_path).expect("open writer");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _probe (n INTEGER);
             INSERT INTO _probe (n) VALUES (1);",
        )
        .expect("seed schema");
        drop(conn);
        let pool = ReaderPool::open(&db_path).expect("ReaderPool::open");
        (dir, pool)
    }

    /// Verification step 1: holding all 8 leases makes a 9th checkout return
    /// the typed `DbError::ReaderPoolExhausted` — never panic / abort. The
    /// pre-#1533 code panicked in release builds (`panic = "abort"`) and
    /// silently worked in debug builds because unwind is allowed.
    #[test]
    fn ninth_checkout_returns_typed_exhaustion_error() {
        let (_dir, pool) = fresh_pool();
        let leases: Vec<_> = (0..READER_POOL_SIZE)
            .map(|_| pool.checkout().expect("initial lease"))
            .collect();

        let started = Instant::now();
        let err = match pool.checkout() {
            Ok(_) => panic!("9th checkout must fail"),
            Err(e) => e,
        };
        let elapsed = started.elapsed();

        match err {
            DbError::ReaderPoolExhausted {
                waited_ms,
                in_use,
                pool_size,
                current_longest_lease_ms,
                historical_longest_lease_ms: _,
            } => {
                assert_eq!(in_use, READER_POOL_SIZE, "all leases must be in use");
                assert_eq!(pool_size, READER_POOL_SIZE);
                assert!(
                    waited_ms >= READER_CHECKOUT_TIMEOUT.as_millis() as u64,
                    "waited_ms must reflect at least the configured timeout"
                );
                // `current_longest_lease_ms` is the age of the longest lease
                // held right now. We held all 8 leases for ~the timeout, so
                // it must be at least that long — and the action signal the
                // review asked us to expose.
                assert!(
                    current_longest_lease_ms >= waited_ms.saturating_sub(50),
                    "current_longest_lease_ms ({current_longest_lease_ms}ms) must reflect a lease held across the wait"
                );
                assert!(
                    elapsed < READER_CHECKOUT_TIMEOUT + Duration::from_millis(250),
                    "elapsed {elapsed:?} must not materially exceed the configured deadline"
                );
            }
            other => panic!("expected DbError::ReaderPoolExhausted, got {other:?}"),
        }

        drop(leases);
    }

    /// Verification step 2: releasing one lease lets the next request
    /// succeed — the condvar notifies a single waiter. We hold the 8 leases
    /// on the main thread, spawn a background thread that calls
    /// `pool.checkout()` (which will park), then drop the leases and verify
    /// the background checkout returns `Ok`.
    #[test]
    fn released_lease_wakes_next_waiter() {
        let (_dir, pool) = fresh_pool();
        let pool = Arc::new(pool);
        let leases = (0..READER_POOL_SIZE)
            .map(|_| pool.checkout().expect("initial lease"))
            .collect::<Vec<_>>();

        let pool_bg = Arc::clone(&pool);
        let handle = thread::spawn(move || {
            // 9th checkout will park until a lease is freed.
            let _guard = pool_bg
                .checkout()
                .expect("woken checkout must succeed after release");
            // Hold `_guard` for a moment to confirm the lease was actually
            // returned before we exit; otherwise the main thread's drop
            // could race with our `Ok` check.
            thread::sleep(Duration::from_millis(10));
        });

        // Hold the leases for a short window, then release them. The
        // background checkout must succeed once we drop them.
        thread::sleep(Duration::from_millis(50));
        drop(leases);

        handle
            .join()
            .expect("background checkout must not panic on wake");
    }

    /// Verification step 3: spurious notifications cannot extend the nominal
    /// timeout. We hold 8 leases, race a 9th checkout, and fire
    /// `notify_one_for_test` repeatedly while no lease is freed — this is
    /// exactly the condvar spurious-wake condition the absolute-deadline
    /// loop exists to handle (issue #1533 review: the original version of
    /// this test fired no notifications and was a tautology).
    #[test]
    fn spurious_notifications_do_not_extend_deadline() {
        let (_dir, pool) = fresh_pool();
        let pool = Arc::new(pool);
        let leases = (0..READER_POOL_SIZE)
            .map(|_| pool.checkout().expect("initial lease"))
            .collect::<Vec<_>>();

        // Background thread fires `notify_one_for_test` repeatedly. This is
        // the condvar-spurious-wake path the absolute-deadline loop closes:
        // a notification arrives, the waiter wakes, finds no lease, and must
        // wait the *remaining* time rather than a fresh
        // `READER_CHECKOUT_TIMEOUT`. Without the absolute deadline, this
        // bug compounded waits past the nominal timeout.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pool_bg = Arc::clone(&pool);
        let stop_bg = Arc::clone(&stop);
        let notifier = thread::spawn(move || {
            while !stop_bg.load(std::sync::atomic::Ordering::Relaxed) {
                pool_bg.notify_one_for_test();
                // Tight loop — without the absolute-deadline guard, every
                // wake would re-arm the timer and the 9th checkout would
                // never time out within the configured deadline.
                std::thread::yield_now();
            }
        });

        let started = Instant::now();
        let err = match pool.checkout() {
            Ok(_) => panic!("checkout must time out under sustained spurious wakes"),
            Err(e) => e,
        };
        let elapsed = started.elapsed();

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        notifier.join().expect("notifier must not panic");
        drop(leases);

        assert!(
            matches!(err, DbError::ReaderPoolExhausted { .. }),
            "expected ReaderPoolExhausted under spurious wakes, got {err:?}"
        );
        // Lower bound: the checkout must actually have waited the full
        // deadline. Without this assertion, an early-return bug (e.g.
        // returning Err on the first spurious wake) would still satisfy the
        // upper-bound check below — review finding #6.
        assert!(
            elapsed >= READER_CHECKOUT_TIMEOUT.saturating_sub(Duration::from_millis(100)),
            "elapsed {elapsed:?} finished before the deadline elapsed — \
             checkout returned early under sustained spurious wakes"
        );
        // Upper bound: the absolute-deadline loop must not let spurious
        // wakes re-arm the timer past the nominal timeout (the bug this
        // whole test exists to pin).
        assert!(
            elapsed < READER_CHECKOUT_TIMEOUT + Duration::from_millis(500),
            "elapsed {elapsed:?} materially exceeded the deadline ({READER_CHECKOUT_TIMEOUT:?}) \
             — absolute-deadline loop regressed?"
        );
    }

    /// Verification step 4: the typed error and the warning log carry only
    /// diagnostic fields — no SQL strings, no parameters, no leaked caller
    /// state. Asserted structurally by checking the field bounds we
    /// documented; the log string is built inside the pool from those
    /// fields only, so a literal-string assertion would couple too tightly
    /// to the format.
    #[test]
    fn exhaustion_error_carries_only_diagnostic_fields() {
        let (_dir, pool) = fresh_pool();
        let leases: Vec<_> = (0..READER_POOL_SIZE)
            .map(|_| pool.checkout().expect("initial lease"))
            .collect();

        let err = match pool.checkout() {
            Ok(_) => panic!("must time out"),
            Err(e) => e,
        };
        match err {
            DbError::ReaderPoolExhausted {
                waited_ms,
                in_use,
                pool_size,
                current_longest_lease_ms,
                historical_longest_lease_ms,
            } => {
                // Each field is bounded — `in_use` and `pool_size` by the
                // static READER_POOL_SIZE; the millisecond fields by the
                // timeout. If any field were repurposed to carry query
                // state, these invariants would break.
                assert!(in_use <= READER_POOL_SIZE);
                assert_eq!(pool_size, READER_POOL_SIZE);
                assert!(waited_ms >= READER_CHECKOUT_TIMEOUT.as_millis() as u64);
                // `historical_longest_lease_ms` is 0 here because every
                // lease was held for less than the slow-lease warning
                // threshold at the moment of checkout; the pool only
                // updates the longest-lease counter in `Drop`. If this
                // test ever holds a lease long enough to bump the counter,
                // that's a diagnostic regression we want to see.
                assert!(
                    historical_longest_lease_ms < READER_CHECKOUT_TIMEOUT.as_millis() as u64,
                    "historical_longest_lease_ms {historical_longest_lease_ms} looks suspiciously like a query payload"
                );
                // `current_longest_lease_ms` IS populated here — that's the
                // whole point of the new field (it was missing in the
                // previous high-water-mark design, which reported 0ms for
                // currently-hung leases).
                let _ = current_longest_lease_ms;
            }
            other => panic!("expected DbError::ReaderPoolExhausted, got {other:?}"),
        }
        drop(leases);
    }

    /// The actionable diagnostic — `current_longest_lease_ms` reflects the
    /// age of a lease still in flight at the moment of timeout. This pins
    /// the fix for the review's "blind to currently hung or deadlocked
    /// leases" finding (issue #1533 review).
    ///
    /// The assertion is `>= 1200ms`, NOT `>= 250ms` as in the v2 version.
    /// The 1-second checkout timeout means every fresh lease is also
    /// ≥1000ms at the moment of failure, so the v2 `>= 250` was vacuously
    /// satisfied by any timeout — even one with a buggy field that
    /// returned just the timeout duration. The tightened bound asserts
    /// the field reflects the slow lease's age (~1250ms: 250ms sleep +
    /// ~1000ms timeout wait), proving the field isn't just echoing the
    /// nominal timeout (review finding #6).
    #[test]
    fn current_longest_lease_age_is_actionable() {
        let (_dir, pool) = fresh_pool();

        // Hold one lease for ~250ms. The remaining 7 are checked out
        // immediately after, then the 9th times out after 1s. At the moment
        // of failure:
        //   * the slow lease has been held for ~1250ms (250ms sleep + ~1s timeout)
        //   * the 7 fresh leases have been held for ~1000ms (just the timeout)
        // `current_longest_lease_ms` must be ≥ the slow lease's age.
        let slow = pool.checkout().expect("first lease");
        thread::sleep(Duration::from_millis(250));

        let leases: Vec<_> = (1..READER_POOL_SIZE)
            .map(|_| pool.checkout().expect("drain remaining leases"))
            .collect();
        let err = match pool.checkout() {
            Ok(_) => panic!("must time out"),
            Err(e) => e,
        };
        match err {
            DbError::ReaderPoolExhausted {
                current_longest_lease_ms,
                ..
            } => {
                assert!(
                    current_longest_lease_ms >= 1200,
                    "current_longest_lease_ms ({current_longest_lease_ms}ms) must reflect the \
                     slow lease's ~1250ms age, NOT just the 1000ms timeout — vacuous assertion \
                     means the field is buggy"
                );
            }
            other => panic!("expected DbError::ReaderPoolExhausted, got {other:?}"),
        }
        drop(leases);
        drop(slow);
    }

    /// `Drop for ReadConnection` removes the lease from the pool's
    /// `in_flight` map. We assert this directly via the test-only
    /// `in_flight_len_for_test` accessor — the timeout's
    /// `current_longest_lease_ms` only counts currently-held leases, so
    /// a stale entry would falsely point at code that already returned
    /// its connection.
    #[test]
    fn drop_removes_lease_from_in_flight() {
        let (_dir, pool) = fresh_pool();
        assert_eq!(
            pool.in_flight_len_for_test(),
            0,
            "fresh pool starts with no in-flight leases"
        );

        let a = pool.checkout().expect("first lease");
        let b = pool.checkout().expect("second lease");
        assert_eq!(
            pool.in_flight_len_for_test(),
            2,
            "two held leases must show up in the in-flight map"
        );

        drop(a);
        assert_eq!(
            pool.in_flight_len_for_test(),
            1,
            "dropping one lease must remove it from the in-flight map"
        );

        drop(b);
        assert_eq!(
            pool.in_flight_len_for_test(),
            0,
            "dropping the last lease must empty the in-flight map"
        );
    }

    /// `historical_longest_lease_ms` updates via `fetch_max` on `Drop`.
    /// Even after the lease is returned, the lifetime high-water mark
    /// remembers it.
    #[test]
    fn historical_longest_lease_records_drop() {
        let (_dir, pool) = fresh_pool();
        let slow = pool.checkout().expect("first lease");
        thread::sleep(Duration::from_millis(120));
        drop(slow);

        // Drain ALL 8 so the 9th times out. The slow lease is gone, but
        // the lifetime high-water mark must still remember it.
        let leases: Vec<_> = (0..READER_POOL_SIZE)
            .map(|_| pool.checkout().expect("drain remaining leases"))
            .collect();
        let err = match pool.checkout() {
            Ok(_) => panic!("must time out"),
            Err(e) => e,
        };
        match err {
            DbError::ReaderPoolExhausted {
                historical_longest_lease_ms,
                ..
            } => {
                assert!(
                    historical_longest_lease_ms >= 120,
                    "historical_longest_lease_ms ({historical_longest_lease_ms}ms) should \
                     reflect the ~120ms lease that just dropped"
                );
            }
            other => panic!("expected DbError::ReaderPoolExhausted, got {other:?}"),
        }
        drop(leases);
    }
}
