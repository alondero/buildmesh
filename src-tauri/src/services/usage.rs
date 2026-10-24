//! Provider usage fetching — piggybacks on CLI credentials stored by each provider.
//!
//! Endpoints are undocumented / reverse-engineered; treat non-200 responses or
//! shape mismatches as "usage unavailable", never as hard errors.
//!
//! The module shape is:
//! - [`types`] — wire shapes (`ProviderUsage`, `UsageWindow`, …). `ts-rs`-derived.
//! - [`outcome`] — internal failure taxonomy + the only place that mints the
//!   wire triple from a [`outcome::UsageOutcome`] (the seam that prevents
//!   per-adapter drift). Visibility-fenced so adapters cannot bypass it.
//! - [`cache`] — 5-minute in-process TTL cache keyed on account identity.
//! - [`last_known`] — 7-day *durable* store of the last reading each provider
//!   reported, used to keep a meter visible when a fetch cannot produce a fresh
//!   reading (ADR-0037). Distinct from [`cache`]: that one keeps a reading
//!   fresh within a process, this one survives restarts.
//! - [`adapter`] — the [`adapter::UsageAdapter`] seam and shared HTTP driver.
//! - [`adapters`] — per-provider drop-in adapters.
//! - [`catalog`] — registry + dispatch + cache lookup.
//!
//! Issue #2528 finished the seam: this module is orchestration only — module
//! declarations and re-exports, pointing at the taxonomy in [`outcome`] and
//! the shared driver in [`adapter`]. A provider's credential read, HTTP
//! request and response parse belongs in its own file under [`adapters`] —
//! `adapters::grok`, `adapters::commandcode`, `adapters::opencode`,
//! `adapters::agy` and their siblings — so a new provider's usage endpoint
//! goes in a new file there rather than here.

pub(crate) mod adapter;
pub(crate) mod adapters;
pub(crate) mod cache;
pub(crate) mod catalog;
pub(crate) mod last_known;
pub(crate) mod outcome;
pub mod types;

// Re-export the wire types the command layer still reaches for, so existing
// `crate::services::usage::{...}` paths keep working. Everything else
// imports from `usage::types` directly — this module's namespace stops
// advertising helpers this module's callers do not use.
pub use types::{ProviderMeters, ProviderUsage};
// Cache stays behind the same `usage::` paths callers already use.
pub use cache::{invalidate_cache, invalidate_provider_cache};
// `fetch_usage` is a fetcher-only driver: internal call sites import it
// directly via `crate::services::usage::adapter::fetch_usage` so the
// `usage::` namespace stops advertising it (issue #1657 step 1: stop
// exporting helpers used only by fetchers). Adapters go through
// `catalog::dispatch(id).fetch` instead.
// The provider helpers that used to live here now live with their owners:
// `home_dir`, `logged_out` and `unavailable` in `types`, `cached_age` in
// `cache`. Internal callers reach them through `super::types::...` or
// `super::cache::...` directly.

/// Re-export the loopback HTTP fixture at the parent module level so
/// `services::freebuff_usage::tests` (a sibling test mod) can share it
/// instead of duplicating the implementation.
#[cfg(test)]
pub(crate) use crate::services::usage::adapter::spawn_loopback;
