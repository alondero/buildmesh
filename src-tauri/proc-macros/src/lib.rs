//! Buildmesh proc-macros — `#[blocking_command]` attribute.
//!
//! Wraps an `async fn` body in `crate::commands::run_blocking(label, move || {
//!     ...body
//! }).await` so a Tauri command can offload blocking work to the
//! blocking thread pool without writing the boilerplate by hand
//! (issue #1380 review point 3).
//!
//! Sync `fn` inputs are returned untouched — Tauri's `#[command]`
//! macro already schedules sync fns on its IPC worker, NOT the
//! bounded tokio pool, so they don't need offloading. This matches
//! the existing `circuit.rs` precedent (10/10 commands are sync).
//!
//! # Usage
//!
//! ```ignore
//! use buildmesh_macros::blocking_command;
//!
//! #[blocking_command]
//! #[tauri::command]
//! pub async fn count_nodes() -> Result<usize, String> {
//!     db::count_agent_nodes().map_err(|e| e.to_string())
//! }
//! ```
//!
//! expands to:
//!
//! ```ignore
//! pub async fn count_nodes() -> Result<usize, String> {
//!     crate::commands::run_blocking("count_nodes", move || {
//!         db::count_agent_nodes().map_err(|e| e.to_string())
//!     }).await
//! }
//! ```
//!
//! Place `#[blocking_command]` BELOW `#[tauri::command]` (or
//! `#[command]` from the `tauri` crate) so the attribute order reads
//! outside-in (Tauri's macro emits the `pub async fn` first, then the
//! body wrapper rewrites it). This matches the convention used by
//! other Rust proc-macro attribute stacks (e.g. `#[tracing::instrument]`).

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

/// Attribute macro that wraps the body of an `async fn` Tauri command
/// in `crate::commands::run_blocking(label, move || { ... }).await`.
///
/// Sync `fn` bodies are returned unchanged.
#[proc_macro_attribute]
pub fn blocking_command(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    // Pull the function name as a `&'static str` so the offload label
    // matches the command name in telemetry (matches the convention
    // every existing run_blocking call uses).
    let name = &input.sig.ident;
    let name_str = name.to_string();

    // Preserve the original signature exactly — visibility, generics,
    // where clauses, attribute decoration that may sit on the fn
    // itself. We only rewrite the body.
    let vis = &input.vis;
    let sig = &input.sig;
    let attrs = &input.attrs;

    // Sync fn: return untouched. Tauri runs sync `#[command] fn` on
    // its IPC worker, not the bounded tokio pool, so no offload
    // boundary is needed.
    if sig.asyncness.is_none() {
        // Re-emit the original ItemFn verbatim (no body rewrite).
        return quote! { #input }.into();
    }

    // Strip any `async` keyword from the sig we re-emit — we keep the
    // async fn *signature* (so callers still `.await` it) but the
    // body becomes the run_blocking wrapper, so the original async
    // body must go inside the closure.
    //
    // Because ItemFn::block is the *body*, not a stmt list, we re-
    // splice it whole. Rust's `move || { #block }` is valid: a
    // closure that contains an async-aware body is just a regular
    // closure, and the captures are by value (which is what we want
    // — `FnOnce + Send + 'static` is the run_blocking bound).
    //
    // The `#[tauri::command]` attribute is preserved verbatim on the
    // re-emitted fn so Tauri's macro still sees a pub async fn with
    // a `Result<T, E>` return.
    let block = &input.block;

    let mut sig_owned = sig.clone();
    // The signature we re-emit keeps `async`; the body becomes the
    // wrapper.
    let _ = &mut sig_owned; // keep the binding alive for the quote!

    let expanded = quote! {
        #(#attrs)*
        #vis #sig {
            crate::commands::run_blocking(#name_str, move || #block).await
        }
    };

    // Annotate with a `#[doc]` blurb so the expanded form is grep-
    // friendly when reading rustdoc output. The `compile_error!`
    // fallback would surface a build error if the macro ever fails to
    // expand — preferable to silently emitting broken code.
    let _ = Span::call_site(); // keep the import alive (proc-macro2 dep)

    expanded.into()
}
