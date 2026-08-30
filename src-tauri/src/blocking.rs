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
