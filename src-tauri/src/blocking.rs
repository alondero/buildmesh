//! Runtime boundary for synchronous work used by async commands and services.
//!
//! Both helpers below also carry an isolated *test* database across the hop:
//! such a database is a `thread_local!` install, so it does not follow work off
//! onto a pool thread — a route handler driven by a test would otherwise
//! resolve the process-global `OnceCell` instead, and either panic as
//! uninitialized or read another test's rows. The shipped binary has no
//! isolated install to capture, so the capture is a no-op in production.

/// What [`capture_test_db`] carries across the offload hop. In production it is
/// the unit type; under `cfg(test)` it is the movable database handle.
#[cfg(test)]
type CapturedTestDb = Option<crate::db::test_support::IsolatedDbHandle>;
#[cfg(not(test))]
type CapturedTestDb = Option<()>;

/// What keeps the pool thread's install alive for the closure's duration.
/// Dropping it uninstalls the database, so it must be bound, never discarded.
#[cfg(test)]
type AdoptedTestDb = Option<crate::db::test_support::IsolatedDbGuard>;
#[cfg(not(test))]
type AdoptedTestDb = Option<()>;

/// Name this thread's isolated database, if it has one.
///
/// Evaluated on the **submitting** thread: the pool thread the closure lands on
/// is exactly the place that has no install to find.
#[cfg(test)]
fn capture_test_db() -> CapturedTestDb {
    crate::db::test_support::installed_handle()
}

#[cfg(not(test))]
fn capture_test_db() -> CapturedTestDb {
    None
}

/// Install the captured handle on the pool thread.
#[cfg(test)]
fn adopt_test_db(handle: CapturedTestDb) -> AdoptedTestDb {
    // The returned guard is what holds the install; callers bind it so it
    // lives exactly as long as the offloaded work.
    Some(crate::db::test_support::adopt(&handle?))
}

#[cfg(not(test))]
fn adopt_test_db(_handle: CapturedTestDb) -> AdoptedTestDb {
    None
}

/// Run a synchronous operation on Tauri's blocking pool while preserving the
/// string error contract used by command and background-service boundaries.
pub(crate) async fn run_blocking<T, F>(label: &'static str, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let handle = capture_test_db();
    tauri::async_runtime::spawn_blocking(move || {
        let _adopted = adopt_test_db(handle);
        f()
    })
    .await
    .map_err(|error| format!("{label} task failed: {error}"))?
}

/// Same pool, but the caller keeps its own error type.
///
/// [`run_blocking`] flattens everything to `String`, which is right for Tauri
/// command boundaries but destroys information the HTTP routes need: the
/// create-PR path carries `PrSourceError`, whose variants map to distinct
/// statuses, and collapsing it to text is how a validation failure once
/// answered 403 and logged the mobile user out (#2190 review).
///
/// Kept separate rather than generalising [`run_blocking`] over `E`: an
/// inferred error type breaks call sites whose closures return a bare
/// `Result<T, _>`, and changing those is unrelated churn.
///
/// `E: From<String>` is what lets an offload failure rejoin the caller's error
/// vocabulary instead of being string-matched — the mechanism issue #2330
/// depends on to keep "the blocking task failed" distinguishable from "the
/// query returned no rows".
pub(crate) async fn run_blocking_typed<T, E, F>(label: &'static str, f: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static + From<String>,
{
    let handle = capture_test_db();
    match tauri::async_runtime::spawn_blocking(move || {
        let _adopted = adopt_test_db(handle);
        f()
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(E::from(format!("{label} task failed: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    //! The offload seam itself. Without this, a route test's private database
    //! stays on the test thread while the handler's query runs on a pool
    //! thread and resolves something else entirely — which is how a "broken
    //! query" test can silently pass by reading the wrong database.
    use super::*;

    #[tokio::test]
    async fn an_isolated_database_follows_the_work_onto_the_blocking_pool() {
        let _db = crate::db::test_support::isolated();
        crate::db::create_mesh("offloaded", "/tmp/bm-offload").unwrap();
        let observed = run_blocking("test_offload", || {
            Ok(crate::db::list_meshes()
                .map(|meshes| meshes.len())
                .unwrap_or(0))
        })
        .await;
        assert_eq!(
            observed.unwrap(),
            1,
            "the pool thread must resolve the test's own database"
        );
    }

    #[tokio::test]
    async fn an_offload_failure_carries_its_label_into_the_callers_error_type() {
        // `run_blocking_typed` gives the caller `E: From<String>`, so the task
        // failure is folded into the caller's vocabulary with its label intact
        // rather than scraped out of an opaque string at the call site.
        let outcome: Result<(), crate::read_outcome::Unavailable> =
            run_blocking_typed("test_label", || -> Result<(), _> { Ok(()) }).await;
        assert_eq!(outcome, Ok(()));

        let converted =
            crate::read_outcome::Unavailable::from("test_label task failed: cancelled".to_string());
        assert!(matches!(
            converted,
            crate::read_outcome::Unavailable::Read(_)
        ));
        assert_ne!(converted, crate::read_outcome::Unavailable::Absent);
    }
}
