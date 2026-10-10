//! Runtime boundary for synchronous work used by async commands and services.

/// Run a synchronous operation on Tauri's blocking pool while preserving the
/// string error contract used by command and background-service boundaries.
pub(crate) async fn run_blocking<T, F>(label: &'static str, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
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
pub(crate) async fn run_blocking_typed<T, E, F>(label: &'static str, f: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static + From<String>,
{
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(result) => result,
        Err(error) => Err(E::from(format!("{label} task failed: {error}"))),
    }
}
