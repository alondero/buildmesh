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
//!   * [`storage`] — disk I/O, in-process cache, atomic write coordination,
//!     and the rule that a corrupt file is never written back.
//!   * [`recovery`] — corruption classification, the last-known-good backup,
//!     and the explicit restore/reset actions (issue #1523).
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
pub mod harness_logins;
pub mod launch_catalog;
pub mod launch_configurations;
pub mod migrations;
pub mod model;
pub mod recovery;
pub mod resolver;
pub(crate) mod secrets;
pub mod spawn_configurations;
pub mod storage;

// Per-feature test files. Shared fixtures (TEST_LOCK, with_temp_dir) live in
// `tests::mod`.
#[cfg(test)]
mod tests;

// `mod preferences` is private to the crate, so a `pub use` in it has
// crate-internal reachability and the `dead_code` / `unused_imports` lints are
// accurate about it. Each slab below therefore names only what a build can
// actually see, and a name reachable only from a `#[cfg(test)]` module carries
// the same `cfg` gate instead of being silenced (issue #2450).

// ----- Re-exports: model ------------------------------------------------

#[cfg(test)]
pub use model::BillingMode;
pub use model::{
    ApiSurface, AppPreferences, HarnessConfigValue, HarnessProfile, ModelTiers,
    PairingVerification, PairingVerificationStatus, ProviderAccount, ProviderPairing,
    ProxiedProviderOrder, SurfaceEndpoint,
};

// ----- Re-exports: storage ----------------------------------------------

pub(crate) use storage::ensure_default_provider_normalized;
pub use storage::{
    app_data_dir, circuit_agent_pool_size, default_provider, generation, health, init,
    issue_spawn_prompt, load, naming_provider, pr_spawn_prompt, preferences_directory, reset,
    restore_backup, reviewer_provider, save, update, worktree_directory,
};
#[cfg(test)]
// allow-test-in-production: the per-thread preferences scratch dir and the load counter
// are shared by the sibling suites.
pub(crate) use storage::{init_for_tests, load_count, reset_for_tests};

// ----- Re-exports: recovery ----------------------------------------------

#[cfg(test)]
pub use recovery::PreferencesStatus;
/// The recovery wire types are only consumed by the Tauri commands and the
/// tests today.
pub use recovery::{PreferencesHealth, RecoveryOutcome};

// ----- Re-exports: resolver ---------------------------------------------

pub(crate) use resolver::effective_pairings;
pub(crate) use resolver::{claude_harness_id_from, minimax_default_tiers};
pub use resolver::{
    compatible_providers_by_harness, compatible_providers_for_harness, default_harness_profiles,
    effective_provider_pairings, endpoint_model_descriptor, first_class_surfaces,
    harness_capabilities_for, harness_profiles, harness_runtime, harness_surface,
    keyed_first_class_catalog, load_snapshot, merge_detected_profiles, minimax_api_key_resolved,
    pairing_compatibility, pairing_for, provider_accounts,
    provider_accounts_with_keys_in_preferences, provider_pairings, provider_surfaces,
    remove_provider_account, remove_provider_pairing, resolve_default_provider,
    resolve_harness_provider, resolve_harness_provider_for, resolve_stored_pairing_and_account,
    resolved_harness_profile, set_account_key_if_absent, set_harness_order,
    set_proxied_provider_order, snapshot_or_default, surface_for_executor, upsert_provider_account,
    upsert_provider_pairing, PreferencesSnapshot,
};
pub use resolver::{CapabilityMaskForResolver, HarnessConfigField, ResolvedCascadeView};

// Reached from the `#[cfg(test)]` suites through `preferences::*`; a release
// build has no reader for any of them. The cascade helpers and the recovery /
// storage types they used to sit here are reached through their own submodule
// paths instead (`resolver::cascade::*`, `super::storage::*`).
#[cfg(test)]
pub(crate) use resolver::BUILTIN_PROVIDER_ACCOUNTS;
#[cfg(test)]
pub use resolver::{is_claude_compatible_id, proxied_order_for};

// ----- Re-exports: compatibility ----------------------------------------

pub use compatibility::{
    harness_default_for, preflight_resolve_provider_env, remove_harness_default,
    resolve_provider_env, upsert_harness_default, validate_harness_default,
};
