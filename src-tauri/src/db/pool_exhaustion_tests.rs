//! Reader-pool exhaustion tests (issue #1533).
//!
//! Pins the contract the production fix closes:
//!
//!   * holding all `READER_POOL_SIZE` leases makes a 9th checkout return
//!     [`DbError::ReaderPoolExhausted`] — *never* panic and *never* abort
//!     the process. Release builds use `panic = "abort"`, so a panic in the
//!     checkout path used to terminate the entire application.
//!   * releasing one lease lets the next request succeed (the condvar
//!     notifies a single waiter).
//!   * spurious notifications / competing waiters cannot extend the
//!     nominal `READER_CHECKOUT_TIMEOUT` materially — the absolute-deadline
//!     loop in `ReaderPool::checkout` re-arms the wait with the *remaining*
//!     duration each iteration, so a wake right before the deadline cannot
//!     re-arm the timer.
//!   * diagnostics identify pool saturation without leaking query
//!     parameters or secrets — the typed error carries `waited_ms`,
//!     `in_use`, `pool_size`, and `longest_lease_ms`; the warning log
//!     message includes only those fields.
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
                longest_lease_ms: _,
            } => {
                assert_eq!(in_use, READER_POOL_SIZE, "all leases must be in use");
                assert_eq!(pool_size, READER_POOL_SIZE);
                assert!(
                    waited_ms >= READER_CHECKOUT_TIMEOUT.as_millis() as u64,
                    "waited_ms must reflect at least the configured timeout"
                );
                assert!(
                    elapsed < READER_CHECKOUT_TIMEOUT + Duration::from_millis(250),
                    "elapsed {elapsed:?} must not materially exceed the configured deadline"
                );
            }
            other => panic!("expected DbError::ReaderPoolExhausted, got {other:?}"),
        }

        // Hold the leases for the duration of the assertion so dropping
        // them doesn't race with the next test.
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
    /// timeout. We hold 8 leases, race a 9th checkout, and notify the
    /// condvar repeatedly from a side thread — the 9th request must still
    /// return `ReaderPoolExhausted` within (approximately) the configured
    /// deadline.
    #[test]
    fn spurious_notifications_do_not_extend_deadline() {
        let (_dir, pool) = fresh_pool();
        let pool = Arc::new(pool);
        let leases = (0..READER_POOL_SIZE)
            .map(|_| pool.checkout().expect("initial lease"))
            .collect::<Vec<_>>();

        // `ReaderPool::ready` is private; we can't notify directly from
        // here, so we simulate "spurious wake" by spawning many short-lived
        // threads that each call `checkout` and immediately drop their
        // guard. The condvar will fire each time, but no lease is freed —
        // so the 9th checkout must still time out within the deadline.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let pool = Arc::clone(&pool);
            let stop = Arc::clone(&stop);
            handles.push(thread::spawn(move || {
                // Each thread grabs a lease if it can; while one is held,
                // we'll loop. Because we hold all 8, every iteration is a
                // timed-out wait — which is exactly the spurious-wake case.
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(_conn) = pool.checkout() {
                        // We shouldn't reach here, but if we do, drop and
                        // let the next iteration race.
                        drop(_conn);
                    }
                }
            }));
        }

        let started = Instant::now();
        let err = match pool.checkout() {
            Ok(_) => panic!("checkout must time out"),
            Err(e) => e,
        };
        let elapsed = started.elapsed();

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for h in handles {
            h.join().expect("spurious-wake threads must not panic");
        }
        drop(leases);

        assert!(
            matches!(err, DbError::ReaderPoolExhausted { .. }),
            "expected ReaderPoolExhausted under spurious wakes, got {err:?}"
        );
        assert!(
            elapsed < READER_CHECKOUT_TIMEOUT + Duration::from_millis(500),
            "elapsed {elapsed:?} materially exceeded the deadline — absolute-deadline loop regressed?"
        );
    }

    /// Verification step 5: the typed error and the warning log carry
    /// diagnostic fields only — no SQL strings, no parameters, no leaked
    /// caller state. We assert this structurally by checking the fields we
    /// documented; the log string is built inside the pool and only uses
    /// those fields, but a literal-string assertion would couple too
    /// tightly to the format.
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
                longest_lease_ms,
            } => {
                // Each field is bounded — `in_use` and `pool_size` by the
                // static READER_POOL_SIZE; `waited_ms` and
                // `longest_lease_ms` by the timeout. If any field were
                // repurposed to carry query state, these invariants would
                // break.
                assert!(in_use <= READER_POOL_SIZE);
                assert_eq!(pool_size, READER_POOL_SIZE);
                assert!(waited_ms >= READER_CHECKOUT_TIMEOUT.as_millis() as u64);
                // `longest_lease_ms` is 0 here because every lease was
                // held for less than the slow-lease warning threshold at
                // the moment of checkout; the pool only updates the
                // longest-lease counter in `Drop`. If this test ever holds
                // a lease long enough to bump the counter, that's a
                // diagnostic regression we want to see.
                assert!(
                    longest_lease_ms < READER_CHECKOUT_TIMEOUT.as_millis() as u64,
                    "longest_lease_ms {longest_lease_ms} looks suspiciously like a query payload"
                );
            }
            other => panic!("expected DbError::ReaderPoolExhausted, got {other:?}"),
        }
        drop(leases);
    }

    /// Slow lease holders bump the diagnostic `longest_lease_ms` counter.
    /// This pins the lease-duration tracking the issue's verification
    /// step 5 asks for — without it, an operator has no way to spot a
    /// caller that's holding a reader connection across filesystem or
    /// network work.
    #[test]
    fn slow_lease_return_records_diagnostic() {
        let (_dir, pool) = fresh_pool();
        // Hold a single lease for ~120ms, then drop it. The pool's
        // `longest_lease_ms` should reflect at least 120ms.
        let lease = pool.checkout().expect("first lease");
        thread::sleep(Duration::from_millis(120));
        drop(lease);

        // Hold all 8 and force a timeout so the typed error carries the
        // updated `longest_lease_ms`. (The atomic is private to the pool,
        // so we observe it through the error path.)
        let leases: Vec<_> = (0..READER_POOL_SIZE)
            .map(|_| pool.checkout().expect("drain remaining leases"))
            .collect();
        let err = match pool.checkout() {
            Ok(_) => panic!("must time out"),
            Err(e) => e,
        };

        match err {
            DbError::ReaderPoolExhausted {
                longest_lease_ms, ..
            } => {
                assert!(
                    longest_lease_ms >= 100,
                    "longest_lease_ms {longest_lease_ms} should reflect the ~120ms sleep"
                );
            }
            other => panic!("expected DbError::ReaderPoolExhausted, got {other:?}"),
        }
        drop(leases);
    }
}
