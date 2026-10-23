//! One preferences read, resolved into the values a derivation needs.
//!
//! Issue #2532. The Spawn Menu derivation used to reach preferences through
//! several independent accessors — [`super::accounts::provider_accounts`],
//! [`super::pairings::provider_pairings`], [`super::harness::harness_profiles`]
//! and friends. Each accessor loaded on its own, so a write landing between
//! two of them could hand the derivation a **torn** view: the accounts from
//! one generation and the pairings from another. The derivation could not
//! notice, because each read looked healthy on its own.
//!
//! This module owns the single read. A [`PreferencesSnapshot`] carries the
//! values the derivation consumes, all from one generation, plus the
//! generation number itself so a caller can key a memo on it.

use super::super::model::{
    AppPreferences, HarnessProfile, PairingVerification, ProviderAccount, ProviderPairing,
    ProxiedProviderOrder,
};
use super::super::storage::{generation, load};
use super::accounts::effective_provider_accounts;
use super::catalog::default_provider_accounts;
use super::harness::{default_harness_profiles, effective_harness_profiles};

/// One consistent view of the preferences a derivation reads.
///
/// The lists come from the same read, so no derivation observes two
/// generations at once. `generation` is the number a caller memoises on: it
/// bumps on each durable preference write, and it only moves forward.
///
/// The stored collections are **views** over [`Self::prefs`] rather than
/// copies of it: one storage, two access paths, so the snapshot and the
/// preferences behind it read the same value. Only the two merged lists
/// ([`Self::accounts`] and [`Self::harness_profiles`]) are owned fields,
/// because they are the code-defined defaults merged with the user's stored
/// rows and have no counterpart in the stored preferences.
#[derive(Debug, Clone, Default)]
pub struct PreferencesSnapshot {
    /// The effective provider accounts — the code-defined defaults with the
    /// user's stored rows merged over them by `id`.
    pub accounts: Vec<ProviderAccount>,
    /// The effective harness profiles — the code-defined defaults with the
    /// user's stored rows merged over them by `id`.
    pub harness_profiles: Vec<HarnessProfile>,
    /// The preferences generation this snapshot was read at.
    pub generation: u64,
    /// The preferences value the lists above were resolved from.
    ///
    /// `None` when the read failed: a derivation then renders no
    /// launch-configuration rows and stamps no permission mode, as it did
    /// before the snapshot existed. The lists are still populated with the
    /// code-defined defaults, so the harness rows themselves are unaffected.
    pub prefs: Option<AppPreferences>,
}

impl PreferencesSnapshot {
    /// The stored **Proxied Provider** pairings.
    pub fn pairings(&self) -> &[ProviderPairing] {
        self.prefs
            .as_ref()
            .map(|prefs| prefs.provider_pairings.as_slice())
            .unwrap_or(&[])
    }

    /// The user's stored spawn-menu harness order.
    pub fn harness_order(&self) -> &[String] {
        self.prefs
            .as_ref()
            .map(|prefs| prefs.harness_order.as_slice())
            .unwrap_or(&[])
    }

    /// The stored per-harness **Proxied Provider** child order.
    pub fn proxied_provider_order(&self) -> &[ProxiedProviderOrder] {
        self.prefs
            .as_ref()
            .map(|prefs| prefs.proxied_provider_order.as_slice())
            .unwrap_or(&[])
    }

    /// The stored pairing-verification records.
    ///
    /// The derivation consults these when it decides whether a Codex route is
    /// launchable, so they belong to the snapshot: leaving them to a read of
    /// their own would be a second preferences read inside one derivation —
    /// the torn read this module exists to remove.
    pub fn pairing_verifications(&self) -> &[PairingVerification] {
        self.prefs
            .as_ref()
            .map(|prefs| prefs.pairing_verifications.as_slice())
            .unwrap_or(&[])
    }
}

/// Read preferences once and resolve the effective view.
///
/// One `load()` feeds the lists, so a derivation does not straddle a write.
///
/// The generation is sampled **before** the read, so the number a caller
/// memoises on is not newer than the data it holds. A write that lands
/// between the two costs one extra derivation later; it does not let a stale
/// menu be served under a fresh key.
pub fn load_snapshot() -> Result<PreferencesSnapshot, String> {
    let generation = generation();
    let mut prefs = load()?;
    let accounts = effective_provider_accounts(&mut prefs);
    let harness_profiles = effective_harness_profiles(&prefs.harness_profiles);
    Ok(PreferencesSnapshot {
        accounts,
        harness_profiles,
        generation,
        prefs: Some(prefs),
    })
}

/// [`load_snapshot`] with the load-failure policy applied once: a failed read
/// is logged and answered with the code-defined defaults.
///
/// This is the one place that decides what a failed preferences read means.
/// The resolver accessors and the Spawn Menu derivation all come through here,
/// so the policy is stated once instead of restated per accessor (issue
/// #2532).
pub fn snapshot_or_default() -> PreferencesSnapshot {
    match load_snapshot() {
        Ok(snapshot) => snapshot,
        Err(e) => {
            tracing::warn!("preferences::snapshot load failed, using defaults: {}", e);
            PreferencesSnapshot {
                accounts: default_provider_accounts(),
                harness_profiles: default_harness_profiles(),
                generation: generation(),
                prefs: None,
            }
        }
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
