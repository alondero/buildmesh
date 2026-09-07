//! Buildmesh-wide preferences, persisted as JSON in `app_data_dir/preferences.json`.
//!
//! This is the **application-level** layer of configuration, distinct from:
//!   - `meshes` DB columns — per-mesh overrides (e.g. `mesh.default_provider`)
//!   - `.claude/settings.json` — per-mesh Claude Code config (worktree.baseRef etc.)
//!
//! Precedence is applied at the call site: per-mesh value → app pref → hardcoded
//! fallback (`anthropic` for providers).
//!
//! The module is split along concern lines:
//!   * [`model`] — wire types (`#[derive(TS)]` structs/enums).
//!   * [`storage`] — disk I/O, in-process cache, atomic write coordination.
//!   * [`migrations`] — legacy `preferences.json` read migration (ADR-0025).
//!   * [`resolver`] — provider catalog, account/pairing merge, harness profile
//!     derivation, default-provider precedence, pairing compatibility.
//!   * [`compatibility`] — spawn-env translation (`ANTHROPIC_*` / `OPENAI_*`)
//!     and harness-default validation against the capability contract.
//!   * [`tests`] — per-feature unit tests (shared fixtures live in `tests/mod.rs`).
//!
//! Every public symbol in the previous monolithic `preferences.rs` is
//! re-exported here so the call sites (`crate::preferences::Foo`) and
//! downstream crates continue to compile unchanged.

pub mod compatibility;
pub mod migrations;
pub mod model;
pub mod resolver;
pub mod storage;

// Per-feature test files. Shared fixtures (TEST_LOCK, with_temp_dir) live in
// `tests::mod`.
#[cfg(test)]
mod tests;

// ----- Re-exports: model ------------------------------------------------

#[allow(unused_imports)]
pub use model::{
    ApiSurface, AppPreferences, BillingMode, HarnessConfigValue, HarnessProfile, ModelTiers,
    PairingVerification, PairingVerificationStatus, ProviderAccount, ProviderPairing,
    ProxiedProviderOrder, SurfaceEndpoint,
};

// ----- Re-exports: storage ----------------------------------------------

#[allow(unused_imports)]
pub(crate) use storage::ensure_default_provider_normalized;
pub use storage::{
    app_data_dir, autopilot_pool_size, default_provider, init, load, naming_provider, save, update,
    worktree_directory,
};
#[cfg(test)]
pub(crate) use storage::{init_for_tests, reset_for_tests};

// ----- Re-exports: resolver ---------------------------------------------

#[allow(unused_imports)]
pub(crate) use resolver::{
    claude_harness_id_from, deepseek_default_tiers, keyed_first_class_template, kimi_default_tiers,
    minimax_default_tiers, BUILTIN_PROVIDER_ACCOUNTS,
};
#[allow(unused_imports)]
pub use resolver::{
    compatible_providers_for_harness, default_harness_profiles, default_provider_accounts,
    effective_provider_pairings, endpoint_model_descriptor, first_class_surfaces,
    harness_capabilities_for, harness_order, harness_profiles, harness_surface,
    is_claude_compatible_id, is_known_harness_id, keyed_first_class_catalog,
    merge_detected_profiles, minimax_api_key_resolved, pairing_compatibility, pairing_for,
    provider_accounts, provider_pairings, provider_surfaces, proxied_order_for,
    proxied_provider_order, remove_provider_account, remove_provider_pairing,
    resolve_default_provider, resolve_harness_provider, resolve_stored_pairing_and_account,
    set_account_key_if_absent, set_harness_order, set_proxied_provider_order, surface_for_executor,
    upsert_provider_account, upsert_provider_pairing,
};
#[allow(unused_imports)]
pub(crate) use resolver::{effective_pairings, pairing_can_potentially_match};

// ----- Re-exports: compatibility ----------------------------------------

#[allow(unused_imports)]
pub use compatibility::{
    harness_default_for, normalize_harness_default, preflight_resolve_provider_env,
    remove_harness_default, resolve_provider_env, upsert_harness_default, validate_harness_default,
};
