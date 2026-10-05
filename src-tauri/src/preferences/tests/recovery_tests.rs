//! Corruption detection, preservation, and explicit recovery (issue #1523).
//!
//! These tests are organised around the issue's acceptance list, one test per
//! claim, because each claim is a distinct promise:
//!
//!   * a corrupt file is never overwritten by a read or an ordinary write;
//!   * the *only* path that replaces it is the explicit reset, and it
//!     archives the original;
//!   * the last-known-good backup really is the last state Buildmesh
//!     accepted, and restoring it brings provider / pairing fields back;
//!   * a valid old-schema file migrates and persists exactly once;
//!   * a forward-version payload is not a destructive fallback.
//!
//! The classification tests call [`classify`] directly — it is pure, so they
//! need no filesystem, no temp dir, and no process-global state. The
//! preservation tests drive the real [`load`] / [`save`] / [`update`] façade
//! through [`with_temp_dir`], because the bug this issue is about lived in
//! the interaction between the cache and the disk, not in either alone.

use super::super::model::AppPreferences;
use super::super::recovery::{self, CorruptionReason};
use super::super::storage::{
    init_for_tests, load, read_state, reset_for_tests, save, update, LoadState,
};
use super::super::{migrations, PreferencesStatus};
use super::{test_dir, with_temp_dir};

/// A legacy-shaped payload: the pre-#1188 `autopilot_pool_size` key and a
/// keyed account still carrying the endpoint fields ADR-0025 migrates.
fn legacy_json() -> String {
    serde_json::json!({
        "autopilot_pool_size": 3,
        "provider_accounts": [{
            "id": "minimax",
            "name": "MiniMax",
            "enabled": true,
            "billing_mode": "pay_as_you_go",
            "claude_compatible": true,
            "api_key": "sk-test-key",
            "base_url": "https://api.minimax.example"
        }]
    })
    .to_string()
}

fn read(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).expect("read preferences.json")
}

// ---------------------------------------------------------------------------
// Classification (pure)
// ---------------------------------------------------------------------------

#[test]
fn malformed_json_is_classified_as_invalid_json() {
    let failure = recovery::classify(b"{not valid json").unwrap_err();
    assert_eq!(failure.reason, CorruptionReason::InvalidJson);
    assert!(
        failure.detail.contains("line") && failure.detail.contains("column"),
        "the detail should locate the failure for the user: {}",
        failure.detail
    );
}

#[test]
fn an_empty_file_is_classified_as_corrupt_not_missing() {
    // A crash between creating the file and the first atomic replace is the
    // classic producer. It is corruption (there are bytes that mean
    // something went wrong), not "no file yet".
    let failure = recovery::classify(b"").unwrap_err();
    assert_eq!(failure.reason, CorruptionReason::InvalidJson);
    assert_eq!(failure.detail, "the file is empty");
    let failure = recovery::classify(b"   \n\t ").unwrap_err();
    assert_eq!(failure.detail, "the file is empty");
}

#[test]
fn non_utf8_bytes_are_classified_as_invalid_json() {
    // A file truncated mid-multi-byte-character, or replaced by a binary
    // blob. `read_to_string` would have surfaced this as an I/O error and
    // the old code's `Err` path; it is corruption the user must be told
    // about, not an unreadable-directory incident.
    let failure = recovery::classify(&[0xff, 0xfe, 0x00, 0x7b]).unwrap_err();
    assert_eq!(failure.reason, CorruptionReason::InvalidJson);
    assert!(
        failure.detail.contains("UTF-8"),
        "unexpected detail: {}",
        failure.detail
    );
}

#[test]
fn a_json_array_is_classified_as_not_an_object() {
    let failure = recovery::classify(b"[1, 2, 3]").unwrap_err();
    assert_eq!(failure.reason, CorruptionReason::NotAnObject);
    assert!(failure.detail.contains("array"), "{}", failure.detail);
}

#[test]
fn a_type_invalid_field_is_classified_as_schema_mismatch() {
    let failure = recovery::classify(br#"{"spawn_configurations":"not-an-array"}"#).unwrap_err();
    assert_eq!(failure.reason, CorruptionReason::SchemaMismatch);
}

#[test]
fn a_corrupt_file_never_leaks_its_contents_into_the_diagnostic() {
    // A preferences file holds plaintext API keys. Both serde error paths
    // are dangerous: a *syntax* error quotes the offending character, a
    // *data* error echoes the offending value ("invalid type: string
    // \"sk-live-…\""). Neither may reach a log line or the UI.
    let secret = "sk-live-DO-NOT-LEAK-0123456789";

    // Valid JSON, wrong type: this is the path through serde's *data*
    // error, whose `Display` would quote the stored string verbatim.
    let payload = format!(r#"{{"spawn_configurations":"{secret}"}}"#);
    let from_data_error = recovery::classify(payload.as_bytes()).unwrap_err();
    assert_eq!(from_data_error.reason, CorruptionReason::SchemaMismatch);
    assert!(
        !from_data_error.detail.contains(secret),
        "schema error leaked the stored value: {}",
        from_data_error.detail
    );

    // Broken syntax: the path through serde's *syntax* error, which quotes
    // the offending character.
    let from_syntax_error =
        recovery::classify(format!(r#"{{"default_provider": {secret}"#).as_bytes()).unwrap_err();
    assert_eq!(from_syntax_error.reason, CorruptionReason::InvalidJson);
    assert!(
        !from_syntax_error.detail.contains(secret),
        "syntax error leaked the source text: {}",
        from_syntax_error.detail
    );
}

#[test]
fn unknown_forward_version_fields_are_ignored_not_rejected() {
    // A file written by a newer Buildmesh whose new fields are all additive
    // must load cleanly and keep the fields this build understands. It is
    // the *destructive* fallback that #1523 is about, so the requirement is
    // "no data loss", not "refuse to start on a newer file".
    let prefs = recovery::classify(
        br#"{"default_provider":"claude","future_flag":{"a":1},"another_unknown":[1,2]}"#,
    )
    .expect("additive unknown fields must load");
    assert_eq!(prefs.default_provider.as_deref(), Some("claude"));
}

// ---------------------------------------------------------------------------
// Preservation: a read and an ordinary write never touch a corrupt file
// ---------------------------------------------------------------------------

#[test]
fn every_corrupt_shape_survives_a_load_and_an_attempted_update_unchanged() {
    let cases: [(&str, &[u8]); 3] = [
        ("invalid", b"{not valid json"),
        ("empty", b""),
        ("shape-invalid", br#"{"spawn_configurations":"not-an-array"}"#),
    ];
    for (label, payload) in cases {
        with_temp_dir(|tmp| {
            let path = tmp.join("preferences.json");
            std::fs::write(&path, payload).unwrap();

            // A read must not repair, normalise, or migrate the file — and
            // must not publish defaults as the authoritative cache.
            let loaded = load().expect("a corrupt read still serves defaults");
            assert_eq!(
                loaded,
                AppPreferences::default(),
                "{label}: reads should fall back to defaults in memory"
            );
            assert_eq!(read(&path), payload, "{label}: a read rewrote the file");

            let error = update(|prefs| prefs.default_provider = Some("must-not-land".into()))
                .expect_err("{label}: an ordinary update must be refused");
            assert!(
                error.contains("PREFERENCES_CORRUPT"),
                "{label}: unstable error code, got {error:?}"
            );
            assert_eq!(
                read(&path),
                payload,
                "{label}: an ordinary update overwrote the corrupt file"
            );

            let error = save(AppPreferences {
                default_provider: Some("must-not-land".into()),
                ..Default::default()
            })
            .expect_err("{label}: an ordinary save must be refused");
            assert!(error.contains("PREFERENCES_CORRUPT"), "{label}: {error:?}");
            assert_eq!(read(&path), payload, "{label}: a save overwrote it");
        });
    }
}

#[test]
fn a_corrupt_file_is_refused_even_after_a_cached_read_populated_the_cache() {
    // The cache is the trap: pre-#1523 a load published defaults and every
    // later write trusted them. The write gate re-reads the file, so a warm
    // cache cannot launder a corrupt file into an overwrite.
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        let payload = b"{\"default_provider\":\"claude\"";
        std::fs::write(&path, payload).unwrap();

        assert!(load().is_ok());
        let error = update(|prefs| prefs.default_provider = Some("x".into())).unwrap_err();
        assert!(error.contains("PREFERENCES_CORRUPT"), "{error:?}");
        assert_eq!(read(&path), payload);
    });
}

#[test]
fn the_health_command_reports_corruption_with_recovery_context() {
    with_temp_dir(|tmp| {
        // Missing is not corrupt.
        let health = super::super::storage::health().unwrap();
        assert_eq!(health.status, PreferencesStatus::Missing);
        assert!(health.corruption.is_none());
        assert_eq!(health.preferences_directory, tmp.display().to_string());

        // A healthy file is healthy, and the directory is reported so the
        // UI can offer "open file location" without a second round trip.
        save(AppPreferences {
            default_provider: Some("claude".into()),
            ..Default::default()
        })
        .unwrap();
        let health = super::super::storage::health().unwrap();
        assert_eq!(health.status, PreferencesStatus::Healthy);
        assert!(health.corruption.is_none());

        // A corrupt file is corrupt, and the report carries the path, the
        // size, and whether a recovery is possible.
        let path = tmp.join("preferences.json");
        std::fs::write(&path, b"not json at all").unwrap();
        let health = super::super::storage::health().unwrap();
        assert_eq!(health.status, PreferencesStatus::Corrupt);
        let info = health.corruption.expect("corrupt must carry detail");
        assert_eq!(info.reason, CorruptionReason::InvalidJson);
        assert_eq!(info.path, path.display().to_string());
        assert_eq!(info.byte_len, 15);
        assert_eq!(info.path, info.path.trim());
        assert!(
            info.backup_available,
            "a successful save must have left a last-known-good backup"
        );
        assert_eq!(
            info.backup_path,
            Some(tmp.join(recovery::BACKUP_SUFFIX).display().to_string())
        );
    });
}

// ---------------------------------------------------------------------------
// Last-known-good backup and recovery
// ---------------------------------------------------------------------------

#[test]
fn a_successful_write_leaves_the_last_known_good_backup() {
    with_temp_dir(|tmp| {
        let backup = tmp.join(recovery::BACKUP_SUFFIX);
        assert!(!backup.exists(), "no write yet, so no backup");

        let saved = AppPreferences {
            default_provider: Some("claude:minimax".into()),
            ..Default::default()
        };
        save(saved.clone()).unwrap();

        assert!(backup.exists(), "the write must refresh the backup");
        let backed_up: AppPreferences =
            serde_json::from_slice(&read(&backup)).expect("the backup must be a valid payload");
        assert_eq!(backed_up.default_provider, Some("claude:minimax".into()));
    });
}

#[test]
#[cfg(unix)]
fn the_backup_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    with_temp_dir(|tmp| {
        save(AppPreferences::default()).unwrap();
        let mode = std::fs::metadata(tmp.join(recovery::BACKUP_SUFFIX))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the backup holds plaintext API keys and must not be group/world readable (mode {mode:o})"
        );
    });
}

#[test]
fn restoring_the_backup_returns_provider_and_pairing_fields() {
    with_temp_dir(|tmp| {
        // A realistic last-known-good state: a keyed provider account with
        // its credential, attached to a harness over a pairing.
        let saved = AppPreferences {
            default_provider: Some("claude:minimax".into()),
            provider_accounts: vec![super::super::ProviderAccount {
                id: "minimax".into(),
                name: "MiniMax".into(),
                enabled: true,
                billing_mode: super::super::BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("sk-recovery-test".into()),
            }],
            provider_pairings: vec![super::super::ProviderPairing {
                harness_id: "claude".into(),
                provider_id: "minimax".into(),
                surface: super::super::ApiSurface::Anthropic,
                base_url: Some("https://api.minimax.example".into()),
                model_tiers: super::super::ModelTiers::default(),
            }],
            ..Default::default()
        };
        save(saved).unwrap();

        // The file goes bad underneath the app, then the app restarts: the
        // cache is dropped, so the next read has to classify the file.
        let path = tmp.join("preferences.json");
        let corrupt = b"{\"provider_accounts\": [";
        std::fs::write(&path, corrupt).unwrap();
        init_for_tests(tmp.clone());

        // Read-only callers keep working off defaults…
        assert_eq!(load().unwrap(), AppPreferences::default());
        // …but the corruption is reported, not silently absorbed.
        assert!(matches!(
            read_state().unwrap(),
            LoadState::Corrupt(_)
        ));

        let outcome = super::super::storage::restore_backup().unwrap();
        assert_eq!(outcome.preferences.default_provider.as_deref(), Some("claude:minimax"));
        assert_eq!(
            outcome.preferences.provider_accounts[0].api_key.as_deref(),
            Some("sk-recovery-test")
        );
        assert_eq!(outcome.preferences.provider_pairings.len(), 1);
        assert_eq!(outcome.preferences.provider_pairings[0].harness_id, "claude");

        // The file is the restored state, and the corrupt bytes are kept.
        let restored: AppPreferences = serde_json::from_slice(&read(&path)).unwrap();
        assert_eq!(restored.default_provider.as_deref(), Some("claude:minimax"));
        let archive = outcome.archive_path.expect("reset/restore must archive");
        assert_eq!(
            read(std::path::Path::new(&archive)),
            corrupt,
            "the archived copy must be the original bytes"
        );

        // Recovery is complete: reads and writes work normally again.
        assert_eq!(
            load().unwrap().default_provider.as_deref(),
            Some("claude:minimax")
        );
        update(|prefs| prefs.reviewer_provider = Some("codex".into())).unwrap();
        assert_eq!(
            super::super::storage::reviewer_provider().as_deref(),
            Some("codex")
        );
    });
}

#[test]
fn restoring_without_a_backup_fails_without_touching_the_file() {
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        let corrupt = b"nonsense";
        std::fs::write(&path, corrupt).unwrap();
        init_for_tests(tmp.clone());

        let error = super::super::storage::restore_backup().unwrap_err();
        assert!(
            error.contains("no last-known-good backup"),
            "unhelpful error: {error}"
        );
        assert_eq!(read(&path), corrupt, "a failed restore must not write");
    });
}

#[test]
fn a_backup_that_no_longer_deserializes_is_refused_rather_than_restored() {
    with_temp_dir(|tmp| {
        // Both files unreadable: restoring would trade one broken file for
        // another and destroy the only remaining copy of the original.
        let path = tmp.join("preferences.json");
        std::fs::write(&path, b"broken primary").unwrap();
        std::fs::write(tmp.join(recovery::BACKUP_SUFFIX), b"broken backup").unwrap();
        init_for_tests(tmp.clone());

        let error = super::super::storage::restore_backup().unwrap_err();
        assert!(
            error.contains("itself unreadable"),
            "unhelpful error: {error}"
        );
        assert_eq!(
            read(&path),
            b"broken primary",
            "a refused restore must not overwrite"
        );
    });
}

#[test]
fn a_backup_that_cannot_be_read_is_not_offered_as_a_recovery() {
    // "Restore" must not be offered as a dead end: a backup that no longer
    // deserializes would trade one broken file for another and destroy the
    // only remaining copy of the user's real settings.
    with_temp_dir(|tmp| {
        std::fs::write(tmp.join("preferences.json"), b"broken primary").unwrap();
        std::fs::write(tmp.join(recovery::BACKUP_SUFFIX), b"{not json").unwrap();
        init_for_tests(tmp.clone());

        assert!(recovery::backup_available(&tmp.join("preferences.json")).is_none());
        let health = super::super::storage::health().unwrap();
        let info = health.corruption.expect("corrupt");
        assert!(
            !info.backup_available,
            "an unreadable backup must not be advertised as restorable"
        );
        assert_eq!(info.backup_path, None);
    });
}

// ---------------------------------------------------------------------------
// Explicit reset
// ---------------------------------------------------------------------------

#[test]
fn an_explicit_reset_is_the_only_path_that_replaces_corrupt_data_and_it_archives() {
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        let corrupt = b"{\"provider_accounts\":[oops";
        std::fs::write(&path, corrupt).unwrap();
        init_for_tests(tmp.clone());

        // Every ordinary route is still refused…
        assert!(save(AppPreferences::default()).is_err());
        assert!(update(|prefs| prefs.default_provider = Some("x".into())).is_err());
        assert_eq!(read(&path), corrupt);

        // …and the explicit reset replaces it while preserving the bytes.
        let outcome = super::super::storage::reset().unwrap();
        assert_eq!(outcome.preferences, AppPreferences::default());
        let archive = outcome.archive_path.expect("reset must archive the original");
        let archive = std::path::PathBuf::from(archive);
        assert_eq!(read(&archive), corrupt, "the archive must be byte-identical");
        assert!(
            archive
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(recovery::CORRUPT_ARCHIVE_PREFIX),
            "unexpected archive name: {:?}",
            archive.file_name()
        );

        // The live file is a valid payload again and the app is writable.
        let rewritten: AppPreferences = serde_json::from_slice(&read(&path)).unwrap();
        assert_eq!(rewritten, AppPreferences::default());
        assert_eq!(super::super::storage::health().unwrap().status, PreferencesStatus::Healthy);
        update(|prefs| prefs.default_provider = Some("claude".into())).unwrap();
    });
}

#[test]
fn a_reset_keeps_the_last_known_good_backup_so_a_reset_is_still_recoverable() {
    with_temp_dir(|tmp| {
        save(AppPreferences {
            default_provider: Some("claude:minimax".into()),
            ..Default::default()
        })
        .unwrap();
        let path = tmp.join("preferences.json");
        std::fs::write(&path, b"broken").unwrap();
        init_for_tests(tmp.clone());

        super::super::storage::reset().unwrap();

        // Reset is the one write that knowingly discards data, so it must
        // not also discard the artifact that could bring it back.
        let backup: AppPreferences =
            serde_json::from_slice(&read(&tmp.join(recovery::BACKUP_SUFFIX))).unwrap();
        assert_eq!(backup.default_provider.as_deref(), Some("claude:minimax"));
    });
}

#[test]
fn successive_resets_do_not_clobber_an_earlier_archive() {
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        let first = super::super::storage::reset().unwrap();
        assert!(first.archive_path.is_none(), "nothing to archive yet");

        std::fs::write(&path, b"first corrupt").unwrap();
        let second = super::super::storage::reset().unwrap();
        std::fs::write(&path, b"second corrupt").unwrap();
        let third = super::super::storage::reset().unwrap();

        let a = second.archive_path.unwrap();
        let b = third.archive_path.unwrap();
        assert_ne!(a, b, "two resets must not share an archive name");
        assert_eq!(read(std::path::Path::new(&a)), b"first corrupt");
        assert_eq!(read(std::path::Path::new(&b)), b"second corrupt");
    });
}

// ---------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------

#[test]
fn a_valid_old_schema_file_migrates_in_memory_and_persists_exactly_once() {
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        let legacy = legacy_json();
        std::fs::write(&path, &legacy).unwrap();

        // The read applies the migration but must not write: persistence is
        // an explicit-write decision, so a read can never destroy the
        // pre-migration bytes of a file that *is* recoverable.
        let loaded = load().unwrap();
        assert_eq!(loaded.circuit_agent_pool_size, Some(3));
        assert_eq!(read(&path), legacy.as_bytes(), "the read persisted early");
        assert!(
            !tmp.join(recovery::BACKUP_SUFFIX).exists(),
            "a read must not fabricate a last-known-good backup"
        );

        // The explicit write persists the migrated shape.
        save(loaded).unwrap();
        let persisted: serde_json::Value =
            serde_json::from_slice(&read(&path)).unwrap();
        assert_eq!(persisted["circuit_agent_pool_size"], serde_json::json!(3));
        assert!(
            persisted.get("autopilot_pool_size").is_none(),
            "the legacy key must be gone"
        );
        assert_eq!(persisted["ad0025_account_pairings_migrated"], true);
        assert!(
            persisted["provider_pairings"]
                .as_array()
                .is_some_and(|p| !p.is_empty()),
            "the legacy account's endpoint must become a pairing"
        );

        // "Exactly once": re-running the migration over what was persisted
        // finds nothing left to do, so the write was not repeated or
        // half-applied.
        let mut again = persisted.clone();
        assert!(
            !migrations::migrate_prefs_json(&mut again),
            "the migration re-ran against an already-migrated payload"
        );
        assert_eq!(again, persisted, "the migration mutated a settled payload");
    });
}

#[test]
fn a_schema_mismatch_never_persists_the_migrated_payload() {
    // The migration rewrites the payload *before* deserialization, so a
    // payload that then fails to deserialize has already been mutated in
    // memory. Nothing may write that mutation back.
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        let payload = br#"{"autopilot_pool_size":7,"spawn_configurations":"not-an-array"}"#;
        std::fs::write(&path, payload).unwrap();

        assert!(load().is_ok(), "reads still serve defaults");
        assert_eq!(
            read(&path),
            payload,
            "a failed deserialization must leave the file byte-identical"
        );
        assert!(!tmp.join(recovery::BACKUP_SUFFIX).exists());
    });
}

#[test]
fn a_forward_version_type_change_is_preserved_not_overwritten() {
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        // A newer Buildmesh changed `spawn_configurations` to an object map.
        let payload = br#"{"spawn_configurations":{"claude":{"model":"opus"}}}"#;
        std::fs::write(&path, payload).unwrap();

        let health = super::super::storage::health().unwrap();
        assert_eq!(health.status, PreferencesStatus::Corrupt);
        assert_eq!(
            health.corruption.unwrap().reason,
            CorruptionReason::SchemaMismatch
        );
        assert_eq!(read(&path), payload, "the forward payload was modified");
    });
}

#[test]
fn an_unreadable_file_is_refused_rather_than_replaced_by_defaults() {
    // A permissions or ACL problem proves nothing about the file's
    // *contents* — it may well be valid — but `fs::rename` can replace a
    // file whose read access is denied. So "cannot read" is a reason to stop,
    // not a reason to write defaults over data the user cannot see.
    //
    // A directory in the file's place produces exactly that: a read error
    // that is *not* `NotFound`, so the "no file yet" path cannot be blamed.
    with_temp_dir(|tmp| {
        let path = tmp.join("preferences.json");
        std::fs::create_dir(&path).unwrap();

        let error = read_state().expect_err("reading a directory must fail");
        assert!(
            !error.contains("PREFERENCES_CORRUPT"),
            "an unreadable file is not a corrupt one, got {error:?}"
        );

        let error = update(|prefs| prefs.default_provider = Some("must-not-land".into()))
            .expect_err("an unreadable file must not be overwritten");
        assert!(
            !error.contains("PREFERENCES_CORRUPT"),
            "an unreadable file is not a corrupt one, got {error:?}"
        );
        assert!(
            path.is_dir(),
            "the unreadable entry was replaced: the file the user cannot \
             read must be left exactly as it is"
        );
    });
}

// ---------------------------------------------------------------------------
// Isolation guard
// ---------------------------------------------------------------------------

#[test]
fn a_corrupt_file_in_one_directory_cannot_affect_another() {
    // The corruption decision is derived from the file on every read, never
    // latched in process state — so two app-data dirs (two profiles, or a
    // test next to a real one) cannot contaminate each other.
    let broken_dir = test_dir();
    std::fs::create_dir_all(&broken_dir).unwrap();
    std::fs::write(broken_dir.join("preferences.json"), b"broken").unwrap();

    with_temp_dir(|_| {
        save(AppPreferences {
            default_provider: Some("claude".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(load().unwrap().default_provider.as_deref(), Some("claude"));
    });

    init_for_tests(broken_dir.clone());
    assert!(matches!(read_state().unwrap(), LoadState::Corrupt(_)));
    reset_for_tests();
    let _ = std::fs::remove_dir_all(&broken_dir);
}
