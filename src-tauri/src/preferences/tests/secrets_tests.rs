//! Tests for provider API keys living in the credential store, not in
//! `preferences.json` (issue #830).
//!
//! Every test drives the production `save` / `update` / `load` boundary and
//! then reads the real bytes on disk, so "the key is not in the file" is
//! asserted against the file rather than against an intermediate struct. The
//! credential store is the per-thread in-memory vault behind
//! [`secrets::test_support`]; the OS-backed wrapper has its own round-trip test
//! at the bottom.

use super::super::model::{AppPreferences, BillingMode, ProviderAccount};
use super::super::secrets::test_support;
use super::super::storage::{init_for_tests, load, restore_backup, save, update};
use super::with_temp_dir;
use std::path::Path;

const KEY: &str = "sk-live-ACCOUNT-SECRET-0001";
const LEGACY_KEY: &str = "sk-live-LEGACY-SECRET-0002";

fn keyed_account(id: &str, key: Option<&str>) -> ProviderAccount {
    ProviderAccount {
        id: id.to_string(),
        name: id.to_string(),
        enabled: true,
        billing_mode: BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: key.map(str::to_string),
    }
}

fn prefs_with(accounts: Vec<ProviderAccount>) -> AppPreferences {
    AppPreferences {
        provider_accounts: accounts,
        ..Default::default()
    }
}

fn file_text(path: &Path) -> String {
    String::from_utf8(std::fs::read(path).expect("read file")).expect("utf-8 file")
}

fn restart(tmp: &Path) {
    // Drops the in-process cache; the credential store survives, exactly as
    // the OS vault survives an app restart.
    init_for_tests(tmp.to_path_buf());
}

fn account_key(id: &str) -> Option<String> {
    load()
        .unwrap()
        .provider_accounts
        .into_iter()
        .find(|a| a.id == id)
        .and_then(|a| a.api_key)
}

#[test]
fn save_keeps_account_and_legacy_keys_out_of_the_file_and_its_backup() {
    with_temp_dir(|tmp| {
        let mut prefs = prefs_with(vec![keyed_account("minimax", Some(KEY))]);
        prefs.minimax_api_key = Some(LEGACY_KEY.to_string());
        save(prefs).unwrap();

        let path = tmp.join("preferences.json");
        let backup = tmp.join("preferences.json.bak");
        for file in [&path, &backup] {
            let text = file_text(file);
            assert!(
                !text.contains(KEY),
                "{} leaked the account key",
                file.display()
            );
            assert!(
                !text.contains(LEGACY_KEY),
                "{} leaked the legacy key",
                file.display()
            );
        }
        let mut held = test_support::stored_values();
        held.sort();
        assert_eq!(held, vec![KEY.to_string(), LEGACY_KEY.to_string()]);

        // The running app still sees the keys, from the cache…
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
        // …and a restart sees them again, hydrated from the store.
        restart(tmp);
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
        assert_eq!(load().unwrap().minimax_api_key.as_deref(), Some(LEGACY_KEY));
    });
}

#[test]
fn update_externalizes_a_key_added_after_startup() {
    with_temp_dir(|tmp| {
        save(prefs_with(vec![keyed_account("custom", None)])).unwrap();
        update(|prefs| prefs.provider_accounts[0].api_key = Some(KEY.to_string())).unwrap();

        assert!(!file_text(&tmp.join("preferences.json")).contains(KEY));
        restart(tmp);
        assert_eq!(account_key("custom").as_deref(), Some(KEY));
    });
}

#[test]
fn a_plaintext_file_from_an_older_build_is_migrated_on_first_load() {
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        let legacy = serde_json::json!({
            "minimax_api_key": LEGACY_KEY,
            "provider_accounts": [
                {"id": "minimax", "name": "MiniMax", "enabled": true,
                 "billing_mode": "pay_as_you_go", "api_key": KEY}
            ]
        })
        .to_string();
        std::fs::write(&path, &legacy).unwrap();
        // An older build also left a last-known-good copy with the same secrets.
        std::fs::write(tmp.join("preferences.json.bak"), &legacy).unwrap();

        // Nothing but a read happens, yet the keys must leave the disk.
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
        assert_eq!(load().unwrap().minimax_api_key.as_deref(), Some(LEGACY_KEY));

        for file in [&path, &tmp.join("preferences.json.bak")] {
            let text = file_text(file);
            assert!(
                !text.contains(KEY),
                "{} still holds the account key",
                file.display()
            );
            assert!(
                !text.contains(LEGACY_KEY),
                "{} still holds the legacy key",
                file.display()
            );
        }
        restart(tmp);
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
    });
}

#[test]
fn an_unavailable_credential_store_keeps_the_key_in_the_file_instead_of_losing_it() {
    with_temp_dir(|tmp| {
        test_support::fail_writes(true);
        save(prefs_with(vec![keyed_account("minimax", Some(KEY))])).unwrap();
        assert!(
            file_text(&tmp.join("preferences.json")).contains(KEY),
            "with no credential store the key must stay in the file so it is not lost"
        );
        restart(tmp);
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));

        // The store comes back: the next write moves the key out of the file.
        test_support::fail_writes(false);
        update(|prefs| prefs.default_provider = Some("minimax".into())).unwrap();
        assert!(!file_text(&tmp.join("preferences.json")).contains(KEY));
        restart(tmp);
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
    });
}

#[test]
fn clearing_a_key_deletes_the_stored_secret() {
    with_temp_dir(|tmp| {
        save(prefs_with(vec![keyed_account("minimax", Some(KEY))])).unwrap();
        assert_eq!(test_support::stored_values(), vec![KEY.to_string()]);

        update(|prefs| prefs.provider_accounts[0].api_key = None).unwrap();

        assert!(test_support::stored_values().is_empty());
        restart(tmp);
        assert_eq!(
            account_key("minimax"),
            None,
            "a cleared key must not come back"
        );
    });
}

#[test]
fn removing_an_account_deletes_its_stored_secret() {
    with_temp_dir(|tmp| {
        save(prefs_with(vec![
            keyed_account("minimax", Some(KEY)),
            keyed_account("custom", Some("sk-other-0003")),
        ]))
        .unwrap();

        update(|prefs| prefs.provider_accounts.retain(|a| a.id != "minimax")).unwrap();

        assert_eq!(
            test_support::stored_values(),
            vec!["sk-other-0003".to_string()]
        );
        restart(tmp);
        assert_eq!(account_key("minimax"), None);
        assert_eq!(account_key("custom").as_deref(), Some("sk-other-0003"));
    });
}

#[test]
fn a_key_present_in_the_file_wins_over_an_older_stored_secret() {
    with_temp_dir(|tmp| {
        save(prefs_with(vec![keyed_account("minimax", Some(KEY))])).unwrap();

        // The user pastes a new key straight into the file (or restores an
        // older file that still carries one): the file is the newer intent.
        let path = tmp.join("preferences.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["provider_accounts"][0]["api_key"] = serde_json::json!("sk-hand-edited-0004");
        std::fs::write(&path, value.to_string()).unwrap();

        restart(tmp);
        assert_eq!(
            account_key("minimax").as_deref(),
            Some("sk-hand-edited-0004")
        );
        assert!(
            !file_text(&path).contains("sk-hand-edited-0004"),
            "and it is migrated out"
        );
    });
}

#[test]
fn restoring_the_backup_rehydrates_keys_from_the_store() {
    with_temp_dir(|tmp| {
        save(prefs_with(vec![keyed_account("minimax", Some(KEY))])).unwrap();
        std::fs::write(tmp.join("preferences.json"), b"{\"provider_accounts\": [").unwrap();
        restart(tmp);

        let outcome = restore_backup().unwrap();
        assert_eq!(
            outcome.preferences.provider_accounts[0].api_key.as_deref(),
            Some(KEY)
        );
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
    });
}

#[test]
fn dev_and_stable_profiles_do_not_share_stored_keys() {
    // The two profiles run side by side as the same OS user, so they share one
    // credential vault; the profile (the data-dir leaf, i.e. the bundle id)
    // must be part of the entry name.
    let root = super::test_dir();
    let stable = root.join("com.alond.buildmesh");
    let dev = root.join("com.alond.buildmesh.dev");
    std::fs::create_dir_all(&stable).unwrap();
    std::fs::create_dir_all(&dev).unwrap();

    init_for_tests(stable.clone());
    save(prefs_with(vec![keyed_account("minimax", Some(KEY))])).unwrap();

    // The dev profile has the same account id but no key of its own.
    init_for_tests(dev.clone());
    save(prefs_with(vec![keyed_account("minimax", None)])).unwrap();
    restart(&dev);
    assert_eq!(
        account_key("minimax"),
        None,
        "dev must not read stable's key"
    );

    // Saving a keyless dev account must not have deleted stable's secret.
    restart(&stable);
    assert_eq!(account_key("minimax").as_deref(), Some(KEY));

    super::super::storage::reset_for_tests();
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(windows)]
#[test]
fn os_backend_round_trips_a_secret_through_the_windows_credential_manager() {
    use super::super::secrets::os_backend;
    if !crate::services::windows_cred::credential_manager_available() {
        eprintln!("SKIP: Windows Credential Manager not accessible from this session");
        return;
    }
    let target = format!(
        "buildmesh-test-prefs-secret-{}",
        uuid::Uuid::new_v4().simple()
    );
    assert_eq!(os_backend::get(&target).unwrap(), None);
    os_backend::set(&target, "sk-os-round-trip-ünï").unwrap();
    assert_eq!(
        os_backend::get(&target).unwrap().as_deref(),
        Some("sk-os-round-trip-ünï")
    );
    os_backend::delete(&target).unwrap();
    assert_eq!(os_backend::get(&target).unwrap(), None);
    // Deleting an absent entry is not an error (clearing a key twice).
    os_backend::delete(&target).unwrap();
}

#[test]
fn a_scrubbed_legacy_file_still_migrates_its_endpoint_into_a_pairing() {
    // ADR-0025 creates a pairing only for an account that has a key. Moving the
    // key out of the file must not change what that migration produces on the
    // next start, or the endpoint the user configured silently vanishes.
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "autopilot_pool_size": 3,
                "provider_accounts": [{
                    "id": "minimax", "name": "MiniMax", "enabled": true,
                    "billing_mode": "pay_as_you_go", "claude_compatible": true,
                    "api_key": KEY, "base_url": "https://api.minimax.example"
                }]
            })
            .to_string(),
        )
        .unwrap();

        let first = load().unwrap();
        assert!(
            !file_text(&path).contains(KEY),
            "the key must have left the file"
        );

        restart(tmp);
        let second = load().unwrap();
        assert_eq!(second.provider_pairings, first.provider_pairings);
        assert!(
            second
                .provider_pairings
                .iter()
                .any(|p| p.harness_id == "claude" && p.provider_id == "minimax"),
            "the keyed account's endpoint must still become a pairing, got {:?}",
            second.provider_pairings
        );
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
    });
}

#[test]
fn a_scrubbed_kimi_companion_still_hands_its_key_to_the_first_class_row() {
    with_temp_dir(|tmp| {
        std::fs::write(
            tmp.join("preferences.json"),
            serde_json::json!({
                "provider_accounts": [{
                    "id": "kimi-via-claude", "name": "Kimi", "enabled": true,
                    "billing_mode": "pay_as_you_go", "claude_compatible": true,
                    "api_key": KEY
                }]
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(account_key("kimi").as_deref(), Some(KEY));
        restart(tmp);
        assert_eq!(account_key("kimi").as_deref(), Some(KEY));
    });
}

#[test]
fn a_refused_migration_leaves_the_plaintext_file_working() {
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        let legacy = serde_json::json!({
            "provider_accounts": [{
                "id": "minimax", "name": "MiniMax", "enabled": true,
                "billing_mode": "pay_as_you_go", "api_key": KEY
            }]
        })
        .to_string();
        std::fs::write(&path, &legacy).unwrap();

        test_support::fail_writes(true);
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            legacy,
            "a store that refuses the key must leave the file untouched"
        );

        // Next start, the store is back: the migration completes.
        test_support::fail_writes(false);
        restart(tmp);
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));
        assert!(!file_text(&path).contains(KEY));
    });
}

#[test]
fn a_plaintext_backup_is_scrubbed_even_when_the_primary_file_is_already_clean() {
    // The primary can be clean while the backup is not (an earlier scrub of the
    // backup failed, or the backup was restored from an older build's copy).
    with_temp_dir(|tmp| {
        save(prefs_with(vec![keyed_account("minimax", Some(KEY))])).unwrap();
        let backup = tmp.join("preferences.json.bak");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
        value["provider_accounts"][0]["api_key"] = serde_json::json!(KEY);
        std::fs::write(&backup, value.to_string()).unwrap();
        assert!(!file_text(&tmp.join("preferences.json")).contains(KEY));

        restart(tmp);
        assert_eq!(account_key("minimax").as_deref(), Some(KEY));

        assert!(
            !file_text(&backup).contains(KEY),
            "the backup still holds the key"
        );
    });
}

#[test]
fn a_stale_key_in_the_backup_never_overwrites_the_newer_stored_key() {
    with_temp_dir(|tmp| {
        save(prefs_with(vec![keyed_account(
            "minimax",
            Some("sk-NEW-0005"),
        )]))
        .unwrap();
        let backup = tmp.join("preferences.json.bak");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
        value["provider_accounts"][0]["api_key"] = serde_json::json!("sk-STALE-0006");
        std::fs::write(&backup, value.to_string()).unwrap();

        restart(tmp);
        assert_eq!(account_key("minimax").as_deref(), Some("sk-NEW-0005"));
        assert!(!file_text(&backup).contains("sk-STALE-0006"));

        restart(tmp);
        assert_eq!(
            account_key("minimax").as_deref(),
            Some("sk-NEW-0005"),
            "scrubbing an old backup replaced the current key"
        );
    });
}

/// Issue #2154 — only the accounts whose key is actually on disk are
/// reported, so the Settings notice lands on the affected card and nowhere
/// else. Both acceptance states in one test, driven through the real
/// `save` / `update` boundary.
///
/// The assertion is on the *reported id list* and on the file's real bytes,
/// not on the in-memory `ProviderAccount` — after hydration a key from the
/// file is indistinguishable from one from the store, which is exactly why
/// the production code reads the file.
///
/// Note the shape of the degraded case: `externalize` re-offers **every**
/// secret on **every** save, so a store that refuses writes strands every
/// keyed account's key on disk, not just the one that changed. That is the
/// honest answer — the notice should name each account whose key is exposed.
#[test]
fn only_the_accounts_whose_key_is_on_disk_are_reported() {
    with_temp_dir(|tmp| {
        // Healthy: the store accepts both keys, so nothing is on disk and
        // Settings must show no notice at all. `anthropic` is a keyless
        // self-auth built-in and must never appear.
        save(prefs_with(vec![
            keyed_account("minimax", Some(KEY)),
            keyed_account("custom", Some("sk-CUSTOM-0007")),
        ]))
        .unwrap();
        restart(tmp);
        assert_eq!(
            super::super::resolver::provider_accounts_with_keys_in_preferences(),
            Vec::<String>::new(),
            "a key the store accepted is not in preferences.json, so no notice is due"
        );

        // Degraded: the store refuses, so the next save strands that key on
        // disk. The file is the evidence, not the in-memory struct.
        test_support::fail_writes(true);
        update(|prefs| {
            let custom = prefs
                .provider_accounts
                .iter_mut()
                .find(|a| a.id == "custom")
                .expect("custom account");
            custom.api_key = Some("sk-CUSTOM-NEW-0008".to_string());
        })
        .unwrap();
        restart(tmp);
        assert!(
            file_text(&tmp.join("preferences.json")).contains("sk-CUSTOM-NEW-0008"),
            "a refused key must stay on disk so it is not lost"
        );
        assert_eq!(
            super::super::resolver::provider_accounts_with_keys_in_preferences(),
            vec!["custom".to_string(), "minimax".to_string()],
            "both keyed accounts are exposed by a store that refuses writes"
        );

        // Recovered: the store accepts the next write, every key leaves the
        // file, and the notice must go away on its own — it must not latch.
        test_support::fail_writes(false);
        update(|prefs| prefs.default_provider = Some("minimax".into())).unwrap();
        restart(tmp);
        assert!(!file_text(&tmp.join("preferences.json")).contains("sk-CUSTOM-NEW-0008"));
        assert_eq!(
            super::super::resolver::provider_accounts_with_keys_in_preferences(),
            Vec::<String>::new(),
            "once the keys are stored, the notice must clear"
        );
    });
}

/// Issue #2154 — a plaintext copy left in the last-known-good backup is still
/// on disk, so it is still reported. Both files are written from the same
/// bytes, so a person told "your key is not in your file" while it sits in
/// the backup has been told a falsehood.
///
/// Each phase calls `load` first, because that is what production does
/// (`get_provider_accounts` reads preferences, and the accounts load calls it
/// before probing) and it is `load` that runs the backup scrub.
#[test]
fn a_key_left_only_in_the_backup_is_still_reported() {
    with_temp_dir(|tmp| {
        save(prefs_with(vec![keyed_account("minimax", Some(KEY))])).unwrap();
        let backup = tmp.join("preferences.json.bak");

        let put_key_in_backup = || {
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
            value["provider_accounts"][0]["api_key"] = serde_json::json!(KEY);
            std::fs::write(&backup, value.to_string()).unwrap();
        };

        put_key_in_backup();
        restart(tmp);
        load().expect("load");
        assert_eq!(
            super::super::resolver::provider_accounts_with_keys_in_preferences(),
            Vec::<String>::new(),
            "the load-time scrub cleared the backup, so nothing is on disk"
        );

        // Now an exposure the scrub cannot clear: the store holds nothing
        // for this slot (the key was cleared, so the entry was deleted) and
        // an older build's backup still carries a plaintext copy. `load`
        // tries to move it out, the store refuses, so it stays on disk — and
        // the notice must appear rather than hide the exposure.
        update(|prefs| prefs.provider_accounts[0].api_key = None).unwrap();
        assert!(test_support::stored_values().is_empty());
        put_key_in_backup();
        test_support::fail_writes(true);
        restart(tmp);
        load().expect("load");
        assert_eq!(
            super::super::resolver::provider_accounts_with_keys_in_preferences(),
            vec!["minimax".to_string()],
            "a key still sitting in the backup must be reported"
        );
        test_support::fail_writes(false);
    });
}

/// Issue #2154 — the deprecated flat `minimax_api_key` is the effective key of
/// the `minimax` account only when that account has none of its own, so the
/// notice must land on `minimax` in that case — and must not fire when the
/// account's own (stored) key is what the app actually uses.
#[test]
fn the_deprecated_flat_key_is_reported_against_the_minimax_account() {
    with_temp_dir(|tmp| {
        let mut prefs = prefs_with(vec![]);
        prefs.minimax_api_key = Some(LEGACY_KEY.to_string());
        save(prefs).unwrap();

        test_support::fail_writes(true);
        update(|prefs| prefs.default_provider = Some("minimax".into())).unwrap();
        restart(tmp);
        // `load` runs the scrub, which the refused store cannot perform — so
        // this is the production ordering, not just a cache reset.
        load().expect("load");
        assert!(
            file_text(&tmp.join("preferences.json")).contains(LEGACY_KEY),
            "a refused legacy key must stay on disk"
        );
        assert_eq!(
            super::super::resolver::provider_accounts_with_keys_in_preferences(),
            vec!["minimax".to_string()],
            "the legacy flat key is reported against the account it feeds"
        );

        // Recovered: the scrub moves it out and the notice clears. This also
        // pins that no stale `true` survives the state change.
        test_support::fail_writes(false);
        restart(tmp);
        load().expect("load");
        assert_eq!(
            super::super::resolver::provider_accounts_with_keys_in_preferences(),
            Vec::<String>::new(),
            "the scrub clears the legacy key once the store is usable again"
        );
    });
}
