//! Mixed-caller stress checks for the reader pool (issue #1533).
//!
//! The unit tests in `db::tests` drive the pool directly: they hold readers,
//! they time a checkout out, they read the counters. The crash this issue
//! exists for, though, happened with *production* callers on *production*
//! threads — the UI's command path, an HTTP route's blocking hop, and a
//! background worker all checking a reader out of the same eight-connection
//! pool at the same time.
//!
//! These checks drive those callers through the same accessor production uses
//! (`db::read_conn()`), concurrently, and pin the two properties that matter:
//!
//! * a saturated pool answers every caller with a **typed busy error** —
//!   never a panic, because the old accessor panicked and a release build
//!   panics with `panic = "abort"`, which took the whole application down
//!   instead of failing one read;
//! * a pool that is *not* saturated serves every concurrent caller, so the
//!   busy answer stays the exception rather than the norm.
//!
//! Run with: cargo test --package buildmesh --lib db::pool_stress_tests

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The callers the mixed load stands in for. The names are the point: a
    /// UI read, an HTTP route read and a background-worker read are three
    /// different threads with three different lifetimes, all competing for
    /// the same pool.
    const CALLERS: &[&str] = &["ui", "http", "worker"];

    /// How many threads each kind of caller contributes.
    ///
    /// More than one per kind, and more in total than the isolated pool holds
    /// (`TEST_READER_POOL_SIZE` readers), so the load genuinely contends:
    /// three kinds of caller queueing for the same few connections is the
    /// shape that starved the pool on the machine that reported the crash.
    const CALLERS_PER_KIND: usize = 3;

    /// Check out every reader the pool holds, so the next checkout has to
    /// wait for the deadline.
    ///
    /// Returns the readers: the caller holds them for the duration of the
    /// check, and dropping them returns them to the pool.
    fn saturate() -> Vec<super::super::ReadConnection<'static>> {
        let database = super::super::test_support::installed_db().expect("the isolated database");
        let size = database.readers.available.lock().len();
        assert!(size > 0, "the isolated database must have readers to hold");
        let mut held = Vec::with_capacity(size);
        for _ in 0..size {
            held.push(
                database
                    .readers
                    .checkout()
                    .expect("a reader is free until the pool is saturated"),
            );
        }
        held
    }

    /// Issue #1533: concurrent callers on a saturated pool all get a typed
    /// busy error, and none of them panics.
    ///
    /// With the old accessor every one of these threads would have panicked
    /// on the checkout — and in a release build that is an abort, not an
    /// unwind: the process would be gone, which is exactly what happened on
    /// the macOS machine that reported the crash.
    #[test]
    fn a_saturated_pool_answers_every_concurrent_caller_with_a_busy_error() {
        let db = super::super::test_support::isolated();
        let held = saturate();
        assert!(
            !held.is_empty(),
            "the isolated database must have at least one reader to hold"
        );

        let busy = AtomicUsize::new(0);
        let served = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for caller in CALLERS {
                for _ in 0..CALLERS_PER_KIND {
                    let handle = db.handle();
                    let busy = &busy;
                    let served = &served;
                    scope.spawn(move || {
                        // A spawned thread does not inherit the test's
                        // thread-local database; it adopts it by handle.
                        let _adopted = super::super::test_support::adopt(&handle);
                        match super::super::read_conn() {
                            Ok(_reader) => {
                                served.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(error) => {
                                assert!(
                                    super::super::error::is_busy_error(&error),
                                    "the {caller} caller must get a typed busy error, got: {error}"
                                );
                                assert!(
                                    error
                                        .to_string()
                                        .contains(super::super::READER_POOL_BUSY_MESSAGE),
                                    "the busy answer must carry the retryable copy, got: {error}"
                                );
                                busy.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    });
                }
            }
        });

        assert_eq!(
            served.load(Ordering::Relaxed),
            0,
            "a saturated pool must not hand out a reader"
        );
        assert_eq!(
            busy.load(Ordering::Relaxed),
            CALLERS.len() * CALLERS_PER_KIND,
            "every concurrent caller must be told the database is busy"
        );

        drop(held);
    }

    /// The other half of the contract: a pool that is *not* saturated serves
    /// every concurrent caller.
    ///
    /// This is what keeps the busy answer honest. A pool that answered `busy`
    /// under ordinary load would turn the retryable answer into the normal
    /// one — and the desktop would start failing reads it should never fail.
    #[test]
    fn an_idle_pool_serves_every_concurrent_caller() {
        let db = super::super::test_support::isolated();
        // Seed one row so the reads have something to answer.
        super::super::create_mesh(
            "pool stress",
            &format!("C:/buildmesh-pool-stress-{}", std::process::id()),
        )
        .unwrap();

        let served = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for caller in CALLERS {
                for _ in 0..CALLERS_PER_KIND {
                    let handle = db.handle();
                    let served = &served;
                    scope.spawn(move || {
                        // A spawned thread does not inherit the test's
                        // thread-local database; it adopts it by handle.
                        let _adopted = super::super::test_support::adopt(&handle);
                        let reader = super::super::read_conn().unwrap_or_else(|error| {
                            panic!("the {caller} caller must be served: {error}")
                        });
                        let count: i64 = reader
                            .query_row("SELECT COUNT(*) FROM meshes", [], |row| row.get(0))
                            .expect("the meshes query answers");
                        assert!(count >= 1, "the seeded mesh must be visible");
                        served.fetch_add(1, Ordering::Relaxed);
                    });
                }
            }
        });

        assert_eq!(
            served.load(Ordering::Relaxed),
            CALLERS.len() * CALLERS_PER_KIND,
            "every concurrent caller must be served by an idle pool"
        );
    }
}
