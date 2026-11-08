use super::*;

#[test]
fn prompts_survive_reopening_and_mutations_preserve_other_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path().into());
    store
        .update(|prefs| prefs.issue_spawn_prompt = Some("Keep this".into()))
        .unwrap();
    let first = save(&store, None, " Review ".into(), " Inspect changes ".into()).unwrap();
    let second = save(&store, None, "Review".into(), "Another prompt".into()).unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(first.name, "Review");
    assert_eq!(first.body, "Inspect changes");
    save(
        &store,
        Some(first.id.clone()),
        "Updated".into(),
        "New body".into(),
    )
    .unwrap();
    let reopened = PreferencesStore::new(dir.path().into());
    assert_eq!(reopened.load().unwrap().saved_prompts[0].body, "New body");
    delete(&reopened, &first.id).unwrap();
    let prefs = PreferencesStore::new(dir.path().into()).load().unwrap();
    assert_eq!(prefs.saved_prompts, vec![second]);
    assert_eq!(prefs.issue_spawn_prompt.as_deref(), Some("Keep this"));
}

#[test]
fn rejects_blank_oversized_and_deleted_prompts_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path().into());
    for (name, body) in [
        (" ".into(), "text".into()),
        ("name".into(), " ".into()),
        ("a".repeat(121), "text".into()),
        ("name".into(), "a".repeat(65537)),
    ] {
        assert!(save(&store, None, name, body).is_err());
    }
    assert!(save(&store, Some("missing".into()), "Name".into(), "Body".into()).is_err());
    assert!(delete(&store, "missing").is_err());
    assert!(!store.preferences_path().exists());
    assert!(store.load().unwrap().saved_prompts.is_empty());
}

#[test]
fn failed_storage_does_not_acknowledge_or_publish_a_prompt() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let store = PreferencesStore::new(file.path().into());
    assert!(save(&store, None, "Name".into(), "Body".into()).is_err());
    assert_eq!(store.revision(), 0);
}

#[test]
fn old_preferences_migrate_without_losing_prompt_overrides() {
    let mut json = serde_json::json!({ "issue_spawn_prompt": "Existing" });
    crate::preferences::migrations::migrate_prefs_json(&mut json);
    assert!(!crate::preferences::migrations::migrate_prefs_json(
        &mut json
    ));
    let prefs: crate::preferences::AppPreferences = serde_json::from_value(json).unwrap();
    assert!(prefs.saved_prompts.is_empty());
    assert_eq!(prefs.issue_spawn_prompt.as_deref(), Some("Existing"));
}
