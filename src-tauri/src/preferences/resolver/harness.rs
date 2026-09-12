//! Harness profile machinery — defaults, merge, ordering, capability lookup.
//!
//! All functions that touch `HarnessProfile` / `harness_profiles` /
//! `harness_order` live here. Catalog and pairing-resolution concerns are
//! kept separate in their own modules.

use super::super::model::HarnessProfile;
use super::super::storage::{load, save};
use crate::agent::capabilities::{capabilities_for, HarnessCapabilities};
use crate::models::Provider;

/// The code-defined harness profiles that always exist regardless of what
/// `preferences.json` stores. Terminal is the first (and, this slice, only)
/// one — a plain-shell harness that injects no provider env, which is why
/// it's the tracer-bullet for the dynamic profile machinery (issue #535).
pub fn default_harness_profiles() -> Vec<HarnessProfile> {
    vec![HarnessProfile {
        id: "terminal".to_string(),
        name: "Terminal".to_string(),
        harness: "terminal".to_string(),
        runtime: None, wsl_distro: None,
    }]
}

/// The effective harness profile list: the code-defined defaults with the
/// user's stored `harness_profiles` merged over them by `id`. A stored
/// profile whose `id` matches a default replaces it (user wins); a stored
/// profile with a new `id` is appended. Defaults are always present, so a
/// built-in like Terminal can never be removed by an empty or partial
/// `preferences.json`.
pub fn harness_profiles() -> Vec<HarnessProfile> {
    let mut profiles = default_harness_profiles();
    let stored = match load() {
        Ok(prefs) => prefs.harness_profiles,
        Err(e) => {
            tracing::warn!("preferences::harness_profiles load failed, using defaults: {}", e);
            Vec::new()
        }
    };
    for profile in stored {
        if let Some(existing) = profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        } else {
            profiles.push(profile);
        }
    }
    profiles
}

pub fn harness_runtime(profile_id: &str) -> Option<crate::models::EnvType> {
    resolved_harness_profile(profile_id)?.runtime
}

pub fn resolved_harness_profile(profile_id: &str) -> Option<HarnessProfile> {
    // Bind the typed value so its lifetime outlives the borrowed
    // `harness_id` slice — issue #1659 item 1 — the original
    // `parse_spawn_option_id` returned slices borrowing `profile_id`
    // directly, so this binding didn't exist before.
    let id = crate::agent::provider::SpawnOptionId::from(profile_id);
    let harness_id = id.harness_id();
    let profiles = harness_profiles();
    let preferred = crate::agent::detection::preferred_profiles(&crate::agent::detection::currently_installed_profiles(profiles.clone()),
        crate::agent::provider::Platform::current(),
        if cfg!(windows) { crate::env::get_default_wsl_distro() } else { None }.as_deref());
    if let Some(profile) = preferred.iter().find(|p| p.id == harness_id) { return Some(profile.clone()); }
    // Canonical ids follow the currently preferred installation; explicit
    // runtime ids below retain their saved profile for existing sessions.
    if let Some(canonical) = crate::agent::detection::canonical_harness(harness_id) {
        let matching: Vec<_> = preferred.iter().filter(|p| p.harness == canonical).collect();
        if matching.len() == 1 { return Some(matching[0].clone()); }
    }
    if let Some(profile) = profiles.iter().find(|p| p.id == harness_id) {
        return Some(profile.clone());
    }
    // Circuit definitions use canonical harness ids. Only an unambiguous
    // installation can supply a runtime for those ids.
    let mut matching = preferred.into_iter().filter(|p| p.harness == harness_id);
    let profile = matching.next()?;
    if matching.next().is_some() { None } else { Some(profile) }
}

/// The user's stored spawn-menu harness order — a list of row ids applied by
/// `agent::provider_menu::order_providers` (issue #573). Empty when never set, in
/// which case the menu keeps its natural derivation order (Terminal still last).
/// A load failure is logged and treated as "no stored order".
pub fn harness_order() -> Vec<String> {
    match load() {
        Ok(prefs) => prefs.harness_order,
        Err(e) => {
            tracing::warn!("preferences::harness_order load failed, using natural order: {}", e);
            Vec::new()
        }
    }
}

/// Persist the spawn-menu harness order (issue #573). `Terminal` is filtered out
/// before storing — it's always forced last by the ordering logic, so keeping it
/// out of the stored list avoids a redundant (and potentially misleading) slot.
///
/// `order` only covers the harnesses installed *right now* (the UI can't render a
/// row for an uninstalled one), so a plain overwrite would silently evict the
/// saved slot of any harness that happens to be uninstalled while the user
/// reorders — breaking the "uninstalled keeps its slot" promise. We instead merge
/// the new order into the stored one via `merge_harness_order`, which keeps each
/// dormant id pinned at its stored slot. Duplicate ids are dropped (first wins).
pub fn set_harness_order(order: Vec<String>) -> Result<(), String> {
    let mut prefs = load()?;
    let incoming = dedupe_keeping_first(order.into_iter().filter(|id| id != "terminal"));
    prefs.harness_order = merge_harness_order(&prefs.harness_order, incoming);
    save(prefs)
}

/// Dedupe an id sequence keeping the first occurrence of each id, preserving
/// order. A malformed caller (or hand-edited prefs) sending `[claude, claude]`
/// would otherwise persist a duplicate that shifts every later harness's index.
fn dedupe_keeping_first(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.filter(|id| seen.insert(id.clone())).collect()
}

/// Merge a user-supplied harness order (which only covers currently-installed
/// harnesses) into the stored order, preserving the saved slot of every *dormant*
/// id — a harness uninstalled right now and so absent from `incoming` (issue
/// #573). Each dormant id holds its stored index; the present ids refill the
/// remaining slots in the user's new order; any brand-new id in `incoming` (never
/// stored before) appends at the end. `incoming` is assumed already deduped and
/// Terminal-free.
fn merge_harness_order(stored: &[String], incoming: Vec<String>) -> Vec<String> {
    let is_dormant = |id: &String| !incoming.iter().any(|x| x == id);
    // Reserve each stored slot: dormant ids keep their place, present ids leave a
    // `None` gap to be refilled by `incoming` below.
    let mut slots: Vec<Option<String>> = stored
        .iter()
        .map(|id| is_dormant(id).then(|| id.clone()))
        .collect();
    let mut gaps = slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.is_none())
        .map(|(i, _)| i)
        .collect::<Vec<_>>()
        .into_iter();
    for id in incoming {
        match gaps.next() {
            Some(i) => slots[i] = Some(id),
            None => slots.push(Some(id)), // brand-new id beyond the stored slots
        }
    }
    slots.into_iter().flatten().collect()
}

/// Merge startup-detected harness profiles into stored preferences (issue #536).
///
/// Additive and idempotent: a detected profile whose `id` is not already stored
/// is appended; existing entries are never overwritten or removed. So a profile
/// the user renamed survives, and re-running the scan every launch (the chosen
/// cadence) only ever *adds* newly-installed tools. Returns the number of
/// profiles added; disk is written only when that is non-zero.
///
/// Detected ids never collide with the code-defined defaults (Terminal), which
/// live outside the stored `harness_profiles` list and are re-merged on read by
/// [`harness_profiles`].
pub fn merge_detected_profiles(detected: Vec<HarnessProfile>) -> Result<usize, String> {
    let mut prefs = load()?;
    let before = prefs.harness_profiles.len();
    for profile in detected {
        if !prefs.harness_profiles.iter().any(|p| p.id == profile.id) {
            prefs.harness_profiles.push(profile);
        }
    }
    let added = prefs.harness_profiles.len() - before;
    if added > 0 {
        save(prefs)?;
    }
    Ok(added)
}

/// Resolve a stored `provider`/profile id to the legacy [`Provider`] executor
/// that should actually spawn it. If the id names a harness profile, the
/// profile's `harness` field is parsed; otherwise the id is parsed directly —
/// the "alongside-legacy" path, so existing enum ids (`"anthropic"`, etc.)
/// still resolve without a matching profile. Unknown ids fall through
/// `Provider::from_db_str`'s Anthropic default (preserving prior behaviour).
///
/// **Composite Spawn Option ids** (issue #575 / ADR-0016): a Proxied
/// Provider id has the shape `<harness>:<provider>` (e.g. `claude:minimax`).
/// Only the *harness* part drives the executor choice — the provider part
/// is just a credential key. We parse via
/// [`crate::agent::provider::SpawnOptionId`] so the legacy bare
/// ids (`"minimax"`, `"kimi"`, custom account ids) still resolve through
/// the same path during the post-#575 migration window, and the post-
/// migration composite ids (`"claude:minimax"`) resolve to the same
/// Anthropic executor as the bare form did.
///
/// Prefer [`resolve_harness_provider_for`] when the caller already holds a
/// typed `SpawnOptionId` — it skips the redundant `&str` → `SpawnOptionId`
/// re-parse that this `&str` overload performs internally (issue #1730
/// review fix-up: previously `services::agent_node::decide_resume` and
/// `autopilot::compatibility::resolve_autopilot_spawn_option` each parsed
/// `new_provider` twice across the two calls).
pub fn resolve_harness_provider(profile_id: &str) -> Provider {
    resolve_harness_provider_for(&crate::agent::provider::SpawnOptionId::from(profile_id))
}

/// Typed entry point — accept an already-parsed [`SpawnOptionId`] and
/// resolve the executor from its harness half (see
/// [`resolve_harness_provider`] for the rationale).
pub fn resolve_harness_provider_for(spawn_option: &crate::agent::provider::SpawnOptionId) -> Provider {
    let harness_id = spawn_option.harness_id();
    match harness_profiles().into_iter().find(|p| p.id == harness_id) {
        Some(profile) => Provider::from_db_str(&profile.harness),
        None => Provider::from_db_str(harness_id),
    }
}

/// Resolve the **capability descriptor** for a harness profile id, used by
/// the application-default validator (issue #1148 step 3). `None` when the
/// profile id doesn't name a known harness (built-in or user-added), so the
/// validator can reject unknown ids at the backend boundary — issue #1148
/// acceptance criteria 5 ("Unknown harness ids … are rejected at the backend
/// boundary").
///
/// Known = `harness_profiles()` carries the id, *or* `Provider::from_db_str`
/// parses it (built-in adapter ids, plus the legacy `"anthropic"` alias).
/// `resolve_harness_provider` already merges both sources and falls back to
/// the Anthropic executor on an unknown — we re-check by feeding the same
/// input into `harness_profiles()` + the built-in id whitelist so unknown
/// ids are refused.
pub fn harness_capabilities_for(profile_id: &str) -> Option<HarnessCapabilities> {
    if !is_known_harness_id(profile_id) {
        return None;
    }
    Some(capabilities_for(resolve_harness_provider(profile_id).adapter()))
}

/// True iff `profile_id` names a known Agent Harness — a built-in adapter id
/// or a stored `HarnessProfile`. Used by [`harness_capabilities_for`] to
/// reject unknown ids without leaking the silent `from_db_str → Anthropic`
/// fallback through the validator (issue #1148 AC #5).
///
/// Built-in ids are matched case-insensitively to mirror
/// [`crate::models::Provider::from_db_str`] (the executor resolver used by
/// `resolve_harness_provider`). Custom profile ids are matched case-
/// sensitively because they are user-defined strings — a user who names a
/// profile `"Claude"` keeps that exact id everywhere.
pub fn is_known_harness_id(profile_id: &str) -> bool {
    let trimmed = profile_id.trim();
    if trimmed.is_empty() {
        return false;
    }
    let normalized = trimmed.to_ascii_lowercase();
    // Built-ins (`BUILTIN_HARNESS_IDS` covers every adapter id plus the
    // legacy `"anthropic"` alias). `contains` short-circuits on the first
    // hit — same semantics as `iter().any()` but cheaper to read.
    if crate::agent::provider::BUILTIN_HARNESS_IDS
        .contains(&normalized.as_str())
    {
        return true;
    }
    // User-stored profiles (custom Claude-compatible profiles like
    // `"deepseek-via-claude"`). `harness_profiles()` is cached via the
    // preferences mutex so this is a single HashMap-style scan on the
    // post-init path.
    harness_profiles().iter().any(|p| p.id == profile_id)
}