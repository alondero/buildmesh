//! Smoke test for the `#[blocking_command]` proc-macro. Compile-only:
//! expands the macro against a fixture and ensures the result is
//! well-formed Rust. Runtime coverage belongs in the main buildmesh
//! crate (`src-tauri/src/services::run_blocking`).

use buildmesh_macros::blocking_command;

// The macro emits `crate::commands::run_blocking(label, move || { ... }).await`,
// so we provide a `commands` module with a stub that mirrors the real
// signature. The macro's smoke test is purely about *expansion* — the
// real offload semantics are exercised in `buildmesh_lib`'s integration
// tests where this stub is replaced by the real `run_blocking`.
mod commands {
    pub async fn run_blocking<T, F>(label: &'static str, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, String> + Send + 'static,
    {
        let _ = label;
        f()
    }
}

// Sanity: the macro on a sync fn returns the body unchanged (no
// offload wrapper — Tauri's IPC worker handles sync commands).
#[blocking_command]
pub fn sync_command() -> Result<u32, String> {
    Ok(42)
}

// Sanity: the macro on an async fn expands to a `run_blocking`
// wrapper. The body still computes the same return value.
#[blocking_command]
pub async fn async_command() -> Result<u32, String> {
    let n: u32 = 7;
    Ok(n * 6)
}

#[tokio::test]
async fn async_command_runs_via_run_blocking() {
    let r = async_command().await;
    assert_eq!(r, Ok(42));
}

#[test]
fn sync_command_returns_value_directly() {
    let r = sync_command();
    assert_eq!(r, Ok(42));
}
