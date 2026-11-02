//! Provider/pairing domain logic — split into focused submodules.
//!
//! The resolver owns everything between the wire types in [`super::model`]
//! and the persistence layer in [`super::storage`]. Concerns are split:
//!
//! | Submodule | Owns |
//! |---|---|
//! | [`harness`]        | Harness profile machinery — defaults, merge, ordering, capability lookup |
//! | [`catalog`]        | Provider catalog — built-in classification, default tiers, surface mapping |
//! | [`accounts`]       | Account list — effective-account resolution, key lookups, mutators |
//! | [`pairings`]       | Pairing resolution — stored-pairing lookups, attach-form defaults, ordering |
//! | [`pairing_compat`] | Pairing compatibility matching — descriptor extractor, decision, predicate |
//! | [`snapshot`]       | One preferences read resolved into the values a derivation needs (issue #2532) |
//! | [`default_provider`] | Default-provider precedence resolver |
//! | [`cascade`]        | Shared per-harness cascade (`explicit > mesh_legacy > application`) + capability mask — single source of truth for the resolver that the spawn path AND the IPC `get_resolved_harness_view` (issue #1656) both call |
//!
//! See the [module-level docs](super) for what concerns each top-level
//! `preferences` submodule owns.

pub mod accounts;
pub mod cascade;
pub mod catalog;
pub mod default_provider;
pub mod harness;
pub mod pairing_compat;
pub mod pairings;
pub mod snapshot;

// `mod preferences` (and so this module) is private to the crate, so each
// re-export here has crate-internal reachability. Names only a `#[cfg(test)]`
// suite reads carry the same gate instead of an `allow` (issue #2450).

// ----- Re-exports: harness -----------------------------------------------

#[cfg(test)]
pub use harness::is_known_harness_id;
pub use harness::{
    default_harness_profiles, harness_capabilities_for, harness_profiles, harness_runtime,
    merge_detected_profiles, resolve_harness_provider, resolve_harness_provider_for,
    resolved_harness_profile, resolved_harness_profile_from, set_harness_order,
};
// The store-scoped twins are reached by the sibling suites that own a store
// (issue #2450) — production reaches the ambient façade above.
#[cfg(test)]
// allow-test-in-production: the store-scoped twins are reached by the sibling suites that
// own a store.
pub use harness::{
    harness_profiles_from, harness_runtime_from, merge_detected_profiles_in, set_harness_order_in,
};

// ----- Re-exports: catalog -----------------------------------------------

pub(crate) use catalog::{claude_harness_id, claude_harness_id_from, minimax_default_tiers};
pub use catalog::{
    first_class_surfaces, harness_surface, is_claude_compatible_id, keyed_first_class_catalog,
    provider_surfaces, surface_for_executor,
};
// The built-in account catalog and the default-account list are read only by
// the catalog suites (the latter through `resolver::`, the former through
// `preferences::*`). The default-tier tables and the per-id account template
// are read inside their own submodule, so they are not re-exported at all
// (issue #2450).
#[cfg(test)]
pub use catalog::default_provider_accounts;
#[cfg(test)]
pub(crate) use catalog::BUILTIN_PROVIDER_ACCOUNTS;

// ----- Re-exports: accounts ----------------------------------------------

pub use accounts::{
    minimax_api_key_resolved, provider_accounts, provider_accounts_with_keys_in_preferences,
    remove_provider_account, set_account_key_if_absent, upsert_provider_account,
};
// The store-scoped twins are reached by the sibling suites that own a store
// (issue #2450).
#[cfg(test)]
// allow-test-in-production: the store-scoped twins are reached by the sibling suites that
// own a store.
pub use accounts::{minimax_api_key_resolved_from, provider_accounts_from};

// ----- Re-exports: pairings ----------------------------------------------

pub(crate) use pairings::effective_pairings;
pub use pairings::{
    compatible_providers_by_harness, compatible_providers_for_harness, effective_provider_pairings,
    pairing_for, remove_provider_pairing, resolve_stored_pairing_and_account,
    set_proxied_provider_order, upsert_provider_pairing,
};
// The store-scoped twins are reached by the sibling suites that own a store
// (issue #2450).
#[cfg(test)]
// allow-test-in-production: the store-scoped twin is reached by the sibling suites that own
// a store.
pub use pairings::proxied_order_for_from;
#[cfg(test)]
// allow-test-in-production: the store-scoped twins are reached by the sibling suites that
// own a store.
pub use pairings::{
    compatible_providers_by_harness_from, compatible_providers_for_harness_from,
    set_proxied_provider_order_in,
};

// ----- Re-exports: pairing_compat ---------------------------------------

pub use pairing_compat::{endpoint_model_descriptor, pairing_compatibility};

// ----- Re-exports: snapshot ----------------------------------------------

pub use snapshot::{load_snapshot, load_snapshot_from, snapshot_or_default, PreferencesSnapshot};

// ----- Re-exports: default_provider --------------------------------------

pub use default_provider::resolve_default_provider;

// ----- Re-exports: cascade ------------------------------------------------
//
// Shared cascade helpers (issue #1656). Both the spawn pipeline
// (`agent::capabilities::resolve_agent_config`) and the IPC resolver
// (`commands::preferences::get_resolved_harness_view`) call into this
// module so the cascade order lives in exactly one place. The IPC command
// imports the functions from `resolver::cascade::*` directly; the spawn
// pipeline reaches `resolve_field` through this re-export.
#[cfg(test)]
pub use cascade::{apply_capability_mask, field_inputs};
pub use cascade::{
    resolve_field, CapabilityMaskForResolver, HarnessConfigField, ResolvedCascadeView,
};
