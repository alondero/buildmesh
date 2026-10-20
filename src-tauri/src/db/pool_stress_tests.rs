//! Mixed-boundary stress checks for the reader pool (issue #1533).
//!
//! The unit tests in `db::tests` drive the pool directly: they hold readers,
//! they time a checkout out, they read the counters. The crash this issue
//! exists for, though, happened with *production* callers on *production*
//! threads — the UI's command path, an HTTP route's blocking hop, and a
//! background worker all checking a reader out of the same eight-connection
//! pool at the same time.
//!
//! These checks drive those callers through the boundaries they really use:
//!
//! * a Tauri command (`commands::mesh::list_meshes`), which offloads to the
//!   blocking pool;
//! * an HTTP route (`http::routes::nodes::list`), which offloads to the
//!   blocking pool and maps the failure to a status;
//! * the circuit worker's interval pass (`services::circuit_triggers`),
//!   which runs on a background thread.
//!
//! All three run *concurrently* against one saturated pool, and the check
//! pins the two properties that matter:
//!
//! * each boundary answers with its own retryable shape — a command error, a
//!   `503`, a skipped pass — and never a panic. A panic in a release build is
//!   `panic = "abort"`, which takes the whole application down instead of
//!   failing one read;
//! * the application is still responsive afterwards: with the readers
//!   released, the same command succeeds.
//!
//! Run with: cargo test --package buildmesh --lib db::pool_stress_tests

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The accessor-level contract: a saturated pool hands every caller the
    /// typed busy error.
    ///
    /// The boundaries below prove the same thing end to end; this one pins
    /// the accessor itself, so a boundary that starts swallowing the error
    /// cannot hide a broken pool.
    #[test]
    fn a_saturated_pool_answers_every_concurrent_caller_with_a_busy_error() {
        let db = crate::db::test_support::isolated();
        let held = saturate();
        assert!(
            !held.is_empty(),
            "the isolated database must have at least one reader to hold"
        );

        let busy = AtomicUsize::new(0);
        let served = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..9 {
                let handle = db.handle();
                let busy = &busy;
                let served = &served;
                scope.spawn(move || {
                    // A spawned thread does not inherit the test's
                    // thread-local database; it adopts it by handle.
                    let _adopted = crate::db::test_support::adopt(&handle);
                    match crate::db::read_conn() {
                        Ok(_reader) => {
                            served.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) => {
                            assert!(
                                crate::db::error::is_busy_error(&error),
                                "every caller must get a typed busy error, got: {error}"
                            );
                            assert!(
                                error
                                    .to_string()
                                    .contains(crate::db::READER_POOL_BUSY_MESSAGE),
                                "the busy answer must carry the retryable copy, got: {error}"
                            );
                            busy.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });

        assert_eq!(
            served.load(Ordering::Relaxed),
            0,
            "a saturated pool must not hand out a reader"
        );
        assert_eq!(
            busy.load(Ordering::Relaxed),
            9,
            "every concurrent caller must be told the database is busy"
        );

        drop(held);
    }

    /// The mixed-boundary acceptance: the UI, the HTTP route and the
    /// background worker, concurrently, against one saturated pool.
    ///
    /// This is the shape that crashed the application — three kinds of
    /// caller, three kinds of thread, one small pool — and it is the shape
    /// the issue's acceptance asks for. The assertions are per boundary:
    /// each one must answer with *its own* retryable shape, and the
    /// application must still serve reads once the pool drains.
    ///
    /// A plain `#[tokio::test]` — the current-thread runtime — is the right
    /// one here: the offload seam captures this thread's isolated database and
    /// adopts it on the blocking pool, and a multi-threaded runtime would run
    /// the spawned tasks on threads that have no such install.
    #[tokio::test]
    async fn the_app_survives_a_saturated_pool_across_its_real_boundaries() {
        let db = crate::db::test_support::isolated();
        crate::db::create_mesh(
            "pool stress",
            &format!("C:/buildmesh-pool-stress-{}", std::process::id()),
        )
        .unwrap();
        let held = saturate();

        // The UI command: a Tauri command that offloads to the blocking pool.
        let ui = tokio::spawn(async { crate::commands::mesh::list_meshes().await });

        // The HTTP route: the same offload, plus the status mapping.
        let http = tokio::spawn(async {
            let request = crate::http::router::ParsedRequest::test_get("/api/nodes");
            crate::http::routes::nodes::list(&request).await
        });

        // The background worker: a real worker pass, on a real worker thread.
        let worker = {
            let handle = db.handle();
            tokio::task::spawn_blocking(move || {
                // A spawned thread does not inherit the test's thread-local
                // database; it adopts it by handle.
                let _adopted = crate::db::test_support::adopt(&handle);
                crate::services::circuit_triggers::run_interval_pass();
            })
        };

        // The UI command answers with the retryable copy, not a panic.
        let ui = ui.await.expect("the UI command task must not panic");
        let error = ui.expect_err("a saturated pool cannot serve the UI command");
        assert!(
            error.contains(crate::db::READER_POOL_BUSY_MESSAGE),
            "the UI command must report the retryable busy copy, got: {error}"
        );

        // The HTTP route answers `503` — transient, so the client retries.
        let http = http.await.expect("the HTTP route task must not panic");
        assert_eq!(
            http.status, "503 Service Unavailable",
            "a busy database must be retryable at the HTTP boundary"
        );

        // The worker pass survives a failed read: it skips the pass instead
        // of panicking, which in a release build would abort the process.
        worker.await.expect("the worker pass task must not panic");

        // The application is still responsive: with the readers released, the
        // same command serves the read it could not serve before.
        drop(held);
        let meshes = crate::commands::mesh::list_meshes()
            .await
            .expect("the pool must serve reads again once the readers are back");
        assert_eq!(meshes.len(), 1, "the seeded mesh must be visible again");
    }

    /// Check out every reader the pool holds, so the next checkout has to
    /// wait for the deadline.
    ///
    /// Returns the readers: the caller holds them for the duration of the
    /// check, and dropping them returns them to the pool.
    fn saturate() -> Vec<crate::db::ReadConnection<'static>> {
        let database = crate::db::test_support::installed_db().expect("the isolated database");
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
}
