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
//! | [`default_provider`] | Default-provider precedence resolver |
//!
//! See the [module-level docs](super) for what concerns each top-level
//! `preferences` submodule owns.

pub mod accounts;
pub mod catalog;
pub mod default_provider;
pub mod harness;
pub mod pairings;
pub mod pairing_compat;

// ----- Re-exports: harness -----------------------------------------------

#[allow(unused_imports)]
pub use harness::{
    default_harness_profiles, harness_capabilities_for, harness_order, harness_profiles,
    is_known_harness_id, merge_detected_profiles, resolve_harness_provider, set_harness_order,
};

// ----- Re-exports: catalog -----------------------------------------------

#[allow(unused_imports)]
pub use catalog::{
    default_provider_accounts, first_class_surfaces, harness_surface,
    is_claude_compatible_id, keyed_first_class_catalog, provider_surfaces, surface_for_executor,
};
#[allow(unused_imports)]
pub(crate) use catalog::{
    BUILTIN_PROVIDER_ACCOUNTS, claude_harness_id, claude_harness_id_from, deepseek_default_tiers,
    kimi_default_tiers, keyed_first_class_template, minimax_default_tiers,
};

// ----- Re-exports: accounts ----------------------------------------------

#[allow(unused_imports)]
pub use accounts::{
    deepseek_api_key_resolved, kimi_api_key_resolved, minimax_api_key_resolved,
    openai_api_key_resolved, openrouter_api_key_resolved, provider_accounts, remove_provider_account,
    remove_provider_pairing, set_account_key_if_absent, upsert_provider_account,
    upsert_provider_pairing,
};

// ----- Re-exports: pairings ----------------------------------------------

#[allow(unused_imports)]
pub use pairings::{
    compatible_providers_for_harness, effective_provider_pairings, pairing_for, provider_pairings,
    proxied_order_for, proxied_provider_order, resolve_stored_pairing_and_account,
    set_proxied_provider_order,
};
#[allow(unused_imports)]
pub(crate) use pairings::effective_pairings;

// ----- Re-exports: pairing_compat ---------------------------------------

#[allow(unused_imports)]
pub use pairing_compat::{pairing_compatibility, endpoint_model_descriptor};
#[allow(unused_imports)]
pub(crate) use pairing_compat::pairing_can_potentially_match;

// ----- Re-exports: default_provider --------------------------------------

#[allow(unused_imports)]
pub use default_provider::resolve_default_provider;