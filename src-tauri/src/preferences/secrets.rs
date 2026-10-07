//! Credential-store seam for provider API keys (issue #830).
//!
//! `ProviderAccount::api_key` and the deprecated `AppPreferences::minimax_api_key`
//! are ordinary fields in memory, so the ~40 callers that read them are
//! untouched. They stop being fields **on disk**: [`externalize`] moves each
//! secret into the OS credential store just before `preferences.json` is
//! serialized, and [`hydrate`] puts them back right after it is parsed. The
//! rest of the app never sees the difference.
//!
//! Rules that keep a key from being lost or leaked:
//! * A key is removed from the file **only after** the store accepted it. With
//!   no store (no logon session, a non-Windows build) the key stays in the file,
//!   exactly as before this seam existed — a degraded mode, never data loss.
//! * A key present in the file wins over the stored one. The file is the newer
//!   intent (a hand edit, a restored backup, a file from an older build), and
//!   the next write moves it into the store.
//! * Entry names carry the profile (the app-data directory's leaf, i.e. the
//!   bundle identifier). The stable and dev builds run side by side as the same
//!   OS user and so share one vault; without the profile in the name a dev save
//!   would overwrite or delete the stable build's keys.
//! * Clearing a key or removing an account deletes its entry. A recovery reset
//!   deliberately does not: like the archived file, the stored keys are what a
//!   user would need to undo it.

use super::model::AppPreferences;
use std::collections::HashSet;

/// Slot of the deprecated flat `minimax_api_key` field.
const LEGACY_MINIMAX_SLOT: &str = "minimax-api-key";

fn account_slot(account_id: &str) -> String {
    format!("provider-account:{account_id}")
}

/// The profile the running instance owns: the app-data directory's leaf, which
/// is the bundle identifier (`com.alond.buildmesh` / `com.alond.buildmesh.dev`).
fn profile() -> String {
    super::storage::app_data_dir()
        .and_then(|dir| {
            dir.file_name()
                .map(|leaf| leaf.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "default".to_string())
}

fn target(slot: &str) -> String {
    format!("buildmesh:{}:{slot}", profile())
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

/// Every slot that currently holds a secret in `prefs`.
fn keyed_slots(prefs: &AppPreferences) -> HashSet<String> {
    let mut slots: HashSet<String> = prefs
        .provider_accounts
        .iter()
        .filter(|account| non_empty(&account.api_key).is_some())
        .map(|account| account_slot(&account.id))
        .collect();
    if non_empty(&prefs.minimax_api_key).is_some() {
        slots.insert(LEGACY_MINIMAX_SLOT.to_string());
    }
    slots
}

/// Calls `visit(owner, field, slot)` for every secret field in raw
/// `preferences.json`: each provider account's `api_key` and the deprecated
/// flat `minimax_api_key`. Works on the JSON rather than the typed struct so it
/// can run before the read-time migrations (see [`hydrate_json`]).
fn visit_secret_fields(
    value: &mut serde_json::Value,
    mut visit: impl FnMut(&mut serde_json::Map<String, serde_json::Value>, &'static str, String),
) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    if let Some(accounts) = root
        .get_mut("provider_accounts")
        .and_then(serde_json::Value::as_array_mut)
    {
        for account in accounts {
            let Some(id) = account
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            if let Some(owner) = account.as_object_mut() {
                visit(owner, "api_key", account_slot(&id));
            }
        }
    }
    visit(root, "minimax_api_key", LEGACY_MINIMAX_SLOT.to_string());
}

fn non_empty_json<'a>(
    owner: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Option<&'a str> {
    owner
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.is_empty())
}

/// Fill the secrets the file does not carry from the credential store, in the
/// parsed JSON, **before** the read-time migrations run: ADR-0025's migration
/// creates an endpoint pairing only for an account that has a key, so a legacy
/// file whose keys were moved out must look exactly like one that still has
/// them.
///
/// Returns `true` when the file still held a plaintext secret — the caller's
/// cue to [`scrub_file`] it. A store read error is logged (never the value) and
/// leaves the key absent for this session; it is not an error, because every
/// other setting must keep working.
pub(crate) fn hydrate_json(value: &mut serde_json::Value) -> bool {
    let mut plaintext_in_file = false;
    visit_secret_fields(value, |owner, field, slot| {
        if non_empty_json(owner, field).is_some() {
            plaintext_in_file = true;
            return;
        }
        match backend::get(&target(&slot)) {
            Ok(Some(secret)) if !secret.is_empty() => {
                owner.insert(field.to_string(), serde_json::Value::String(secret));
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%slot, %error, "credential store read failed; the key is unavailable this session");
            }
        }
    });
    plaintext_in_file
}

/// Move the plaintext secrets of the file at `path` into the credential store
/// and blank them in place.
///
/// Deliberately a surgical edit, not a re-save: a plain read must not migrate
/// the file's shape or fabricate a last-known-good backup (the read-never-
/// persists rule in `storage`), so everything but the secret fields is left as
/// it was. A secret the store refuses stays in the file. A missing or
/// unparseable file is left alone — repairing it is not this function's job.
///
/// `authoritative` is true for `preferences.json`, whose key is the user's
/// current intent and replaces the stored one. It is false for the
/// last-known-good backup, which is older: there a key is only stored when the
/// store has none, and is otherwise just dropped, so an old backup can never
/// overwrite a newer key.
pub(crate) fn scrub_file(path: &std::path::Path, authoritative: bool) -> Result<(), String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return Ok(());
    };
    let mut changed = false;
    visit_secret_fields(&mut value, |owner, field, slot| {
        let Some(secret) = non_empty_json(owner, field).map(str::to_owned) else {
            return;
        };
        if !authoritative
            && matches!(backend::get(&target(&slot)), Ok(Some(stored)) if !stored.is_empty())
        {
            owner.insert(field.to_string(), serde_json::Value::Null);
            changed = true;
            return;
        }
        match backend::set(&target(&slot), &secret) {
            Ok(()) => {
                owner.insert(field.to_string(), serde_json::Value::Null);
                changed = true;
            }
            Err(error) => {
                tracing::warn!(%slot, %error, "credential store write failed; the key stays in preferences.json");
            }
        }
    });
    if !changed {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("failed to serialize scrubbed preferences: {error}"))?;
    super::recovery::persist_bytes(path, &bytes)
}

/// `preferences.json` bytes with the stored keys put back in, for a copy that
/// is meant to be full fidelity (an automatic snapshot, an export with
/// redaction switched off): its promise is that restoring it, possibly on
/// another machine, brings the keys with it. A file that cannot be parsed is
/// returned unchanged — that is the bundler's call to make, not this one's.
pub(crate) fn with_keys_inlined(raw: Vec<u8>) -> Vec<u8> {
    let mut value = match serde_json::from_slice::<serde_json::Value>(&raw) {
        Ok(value) if value.is_object() => value,
        _ => return raw,
    };
    hydrate_json(&mut value);
    serde_json::to_vec_pretty(&value).unwrap_or(raw)
}

/// Struct-level counterpart of [`hydrate_json`], for a backup restored through
/// `recovery` (which classifies the bytes itself). A backup is always written in
/// the current shape, so skipping the pre-migration ordering is safe here.
pub(crate) fn hydrate(prefs: &mut AppPreferences) -> bool {
    let mut plaintext_in_file = false;
    let mut fill = |field: &mut Option<String>, slot: String| {
        if non_empty(field).is_some() {
            plaintext_in_file = true;
            return;
        }
        match backend::get(&target(&slot)) {
            Ok(Some(value)) if !value.is_empty() => *field = Some(value),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%slot, %error, "credential store read failed; the key is unavailable this session");
            }
        }
    };
    for account in &mut prefs.provider_accounts {
        fill(&mut account.api_key, account_slot(&account.id));
    }
    fill(&mut prefs.minimax_api_key, LEGACY_MINIMAX_SLOT.to_string());
    plaintext_in_file
}

/// The copy of `prefs` that may be serialized to `preferences.json`.
///
/// Stores every secret and strips the ones the store accepted. `previous` is
/// the state being replaced: a secret it held that `prefs` no longer does (a
/// cleared key, a removed account) is deleted from the store.
pub(crate) fn externalize(
    prefs: &AppPreferences,
    previous: Option<&AppPreferences>,
) -> AppPreferences {
    let mut on_disk = prefs.clone();
    let move_out = |field: &mut Option<String>, slot: String| {
        let Some(value) = non_empty(field) else {
            return;
        };
        match backend::set(&target(&slot), value) {
            Ok(()) => *field = None,
            Err(error) => {
                tracing::warn!(%slot, %error, "credential store write failed; the key stays in preferences.json");
            }
        }
    };
    for account in &mut on_disk.provider_accounts {
        let slot = account_slot(&account.id);
        move_out(&mut account.api_key, slot);
    }
    move_out(
        &mut on_disk.minimax_api_key,
        LEGACY_MINIMAX_SLOT.to_string(),
    );

    if let Some(previous) = previous {
        let live = keyed_slots(prefs);
        for slot in keyed_slots(previous).difference(&live) {
            if let Err(error) = backend::delete(&target(slot)) {
                tracing::warn!(%slot, %error, "credential store delete failed; a stale key entry remains");
            }
        }
    }
    on_disk
}

#[cfg(all(windows, not(test)))]
use os_backend as backend;
#[cfg(test)]
use test_vault as backend;
#[cfg(all(not(windows), not(test)))]
use unavailable as backend;

/// Windows Credential Manager, through the hand-rolled FFI in
/// `services::windows_cred`. Compiled under test too so its string↔blob
/// mapping has a real round-trip test even though the other tests use the
/// in-memory vault.
#[cfg(windows)]
pub(crate) mod os_backend {
    use crate::services::usage::types::UsageError;
    use crate::services::windows_cred;

    pub(crate) fn get(target: &str) -> Result<Option<String>, String> {
        match windows_cred::read(target) {
            Ok(blob) => String::from_utf8(blob)
                .map(Some)
                .map_err(|_| format!("{target} is not valid UTF-8")),
            // `windows_cred::read` reports every failed read, including "no
            // such entry", as `NoCredential`.
            Err(UsageError::NoCredential(_)) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn set(target: &str, value: &str) -> Result<(), String> {
        windows_cred::write(target, value.as_bytes()).map_err(|error| error.to_string())
    }

    /// Idempotent: a missing entry is success (`windows_cred::delete` contract).
    pub(crate) fn delete(target: &str) -> Result<(), String> {
        windows_cred::delete(target).map_err(|error| error.to_string())
    }
}

/// No credential store on this platform: reads find nothing and writes fail, so
/// [`externalize`] leaves every key in the file.
#[cfg(all(not(windows), not(test)))]
mod unavailable {
    pub(crate) fn get(_target: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    pub(crate) fn set(_target: &str, _value: &str) -> Result<(), String> {
        Err("no credential store on this platform".to_string())
    }

    pub(crate) fn delete(_target: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Per-thread in-memory vault, mirroring the per-thread preferences cache so
/// parallel tests never share secrets and never touch the real Credential
/// Manager.
#[cfg(test)]
mod test_vault {
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    pub(super) struct Vault {
        pub(super) entries: HashMap<String, String>,
        pub(super) fail_writes: bool,
    }

    thread_local! {
        pub(super) static VAULT: RefCell<Vault> = RefCell::new(Vault::default());
    }

    pub(crate) fn get(target: &str) -> Result<Option<String>, String> {
        Ok(VAULT.with(|vault| vault.borrow().entries.get(target).cloned()))
    }

    pub(crate) fn set(target: &str, value: &str) -> Result<(), String> {
        VAULT.with(|vault| {
            let mut vault = vault.borrow_mut();
            if vault.fail_writes {
                return Err("credential store unavailable".to_string());
            }
            vault.entries.insert(target.to_string(), value.to_string());
            Ok(())
        })
    }

    pub(crate) fn delete(target: &str) -> Result<(), String> {
        VAULT.with(|vault| {
            let mut vault = vault.borrow_mut();
            if vault.fail_writes {
                return Err("credential store unavailable".to_string());
            }
            vault.entries.remove(target);
            Ok(())
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::test_vault::VAULT;

    /// Forget every stored secret and any injected failure.
    pub(crate) fn reset() {
        VAULT.with(|vault| *vault.borrow_mut() = Default::default());
    }

    /// Make every credential-store write fail (simulates no logon session).
    pub(crate) fn fail_writes(fail: bool) {
        VAULT.with(|vault| vault.borrow_mut().fail_writes = fail);
    }

    /// Store `value` as the key of provider account `id` in the current profile,
    /// as if a build had saved it there.
    pub(crate) fn seed_account_secret(id: &str, value: &str) {
        super::backend::set(&super::target(&super::account_slot(id)), value).unwrap();
    }

    /// Every secret value currently held by the in-memory credential store.
    pub(crate) fn stored_values() -> Vec<String> {
        VAULT.with(|vault| vault.borrow().entries.values().cloned().collect())
    }
}
