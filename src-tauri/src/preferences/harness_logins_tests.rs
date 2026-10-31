//! Harness Login rules: ids, labels, resolution, runtime refusal, home
//! seeding, and how a login travels through Launch Configurations into the
//! frozen Resolved Launch Plan.

use super::*;
use crate::models::EnvType;
use crate::preferences::{launch_configurations, spawn_configurations, AppPreferences};

fn login(id: &str, label: &str) -> HarnessLogin {
    HarnessLogin {
        id: id.into(),
        harness_id: "codex".into(),
        label: label.into(),
        home: format!(r"C:\Users\u\.codex-accounts\{id}"),
    }
}

fn prefs_with(logins: Vec<HarnessLogin>) -> AppPreferences {
    AppPreferences {
        harness_logins: logins,
        ..Default::default()
    }
}

#[test]
fn ids_are_slugs_that_never_collide_or_claim_the_default_id() {
    let existing = vec![login("work", "Work")];
    assert_eq!(unique_id_for("Personal Pro", &existing), "personal-pro");
    assert_eq!(unique_id_for("Work", &existing), "work-2");
    assert_eq!(unique_id_for("  Ünïcode!! ", &[]), "n-code");
    assert_eq!(unique_id_for("default", &[]), "login");
    assert_eq!(unique_id_for("***", &[]), "login");
}

#[test]
fn labels_are_trimmed_bounded_and_cannot_impersonate_the_default() {
    assert_eq!(validate_label("  Work  ").unwrap(), "Work");
    assert!(validate_label("   ").is_err());
    assert!(validate_label("Default").is_err());
    assert!(validate_label(&"x".repeat(41)).is_err());
}

#[test]
fn resolving_a_login_never_falls_back_to_another_account() {
    let prefs = prefs_with(vec![login("work", "Work")]);
    assert_eq!(resolve_login(&prefs, "codex", None).unwrap(), None);
    assert_eq!(
        resolve_login(&prefs, "codex", Some("default")).unwrap(),
        None
    );
    assert_eq!(resolve_login(&prefs, "codex", Some("  ")).unwrap(), None);
    let work = resolve_login(&prefs, "codex", Some("work"))
        .unwrap()
        .unwrap();
    assert_eq!(work.label, "Work");
    assert!(work.home.ends_with("work"));
    let missing = resolve_login(&prefs, "codex", Some("gone")).unwrap_err();
    assert!(missing.contains("no longer exists"), "{missing}");
    let other = resolve_login(&prefs, "anthropic", Some("work")).unwrap_err();
    assert!(other.contains("Only Codex"), "{other}");
}

#[test]
fn only_native_launches_can_use_an_additional_login() {
    let work = HarnessLoginRef::from(&login("work", "Work"));
    assert_eq!(runtime_refusal(&work, EnvType::Windows), None);
    let wsl = runtime_refusal(&work, EnvType::Wsl).unwrap();
    assert!(wsl.contains("only available for native Windows"), "{wsl}");
    assert!(runtime_refusal(&work, EnvType::WindowsInterop).is_some());
}

#[test]
fn seeding_copies_settings_once_and_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let default_home = root.path().join(".codex");
    std::fs::create_dir_all(&default_home).unwrap();
    std::fs::write(default_home.join("config.toml"), "model = \"gpt\"\n").unwrap();
    std::fs::write(default_home.join("AGENTS.md"), "be terse\n").unwrap();
    std::fs::write(default_home.join("auth.json"), "{\"secret\":true}").unwrap();

    let home = managed_home_for(root.path(), "work");
    assert_eq!(home, root.path().join(".codex-accounts").join("work"));
    seed_home(&home, Some(&default_home)).unwrap();
    assert_eq!(
        std::fs::read_to_string(home.join("config.toml")).unwrap(),
        "model = \"gpt\"\n"
    );
    assert!(home.join("AGENTS.md").is_file());
    assert!(
        !home.join("auth.json").exists(),
        "the default login's credential must never be copied"
    );

    std::fs::write(home.join("config.toml"), "model = \"mine\"\n").unwrap();
    seed_home(&home, Some(&default_home)).unwrap();
    assert_eq!(
        std::fs::read_to_string(home.join("config.toml")).unwrap(),
        "model = \"mine\"\n",
        "a login's own settings survive a second seed"
    );
}

#[test]
fn a_pinned_configuration_freezes_its_login_into_the_plan() {
    let mut prefs = prefs_with(vec![login("work", "Work")]);
    let saved = spawn_configurations::validate(spawn_configurations::SpawnConfiguration {
        id: "launch/codex-work".into(),
        name: "Codex on Work".into(),
        spawn_option_id: "codex".into(),
        login_id: Some(" work ".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(saved.login_id.as_deref(), Some("work"));
    prefs.spawn_configurations.push(saved);

    let plan = launch_configurations::resolve_for_edit(&prefs, "launch/codex-work").unwrap();
    assert_eq!(
        plan.harness_login.as_ref().map(|l| l.id.as_str()),
        Some("work")
    );

    // Unpinned configurations and bare options launch on the default login.
    let plan = launch_configurations::resolve_for_edit(&prefs, "codex").unwrap();
    assert_eq!(plan.harness_login, None);

    // Removing the login makes the pin fail loudly instead of billing the
    // default account.
    prefs.harness_logins.clear();
    let error = launch_configurations::resolve_for_edit(&prefs, "launch/codex-work").unwrap_err();
    assert!(error.contains("no longer exists"), "{error}");
}

#[test]
fn a_login_cannot_be_pinned_on_another_harness_or_a_provider_route() {
    let other = spawn_configurations::validate(spawn_configurations::SpawnConfiguration {
        id: "launch/claude-work".into(),
        name: "Claude".into(),
        spawn_option_id: "claude".into(),
        login_id: Some("work".into()),
        ..Default::default()
    })
    .unwrap_err();
    assert!(other.contains("Only Codex"), "{other}");
    let routed = spawn_configurations::validate(spawn_configurations::SpawnConfiguration {
        id: "launch/codex-minimax".into(),
        name: "MiniMax".into(),
        spawn_option_id: "codex:minimax".into(),
        login_id: Some("work".into()),
        ..Default::default()
    })
    .unwrap_err();
    assert!(routed.contains("Provider Route"), "{routed}");
    // The default id is the same as no pin.
    let default = spawn_configurations::validate(spawn_configurations::SpawnConfiguration {
        id: "launch/codex-default".into(),
        name: "Default".into(),
        spawn_option_id: "codex".into(),
        login_id: Some("default".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(default.login_id, None);
}

#[test]
fn the_menu_pick_freezes_defaults_and_overrides_a_pin() {
    let mut prefs = prefs_with(vec![login("work", "Work"), login("home", "Home")]);
    // "Spawn with defaults" under a login: no configuration, so the defaults
    // are resolved and frozen with the login.
    let snapshot = spawn_configurations::with_login_pick(&prefs, "codex", None, Some("work"))
        .unwrap()
        .unwrap();
    let plan = snapshot.resolved.expect("defaults are frozen as a plan");
    assert_eq!(plan.spawn_option_id, "codex");
    assert_eq!(plan.harness_login.unwrap().id, "work");

    // A saved configuration picked under another login launches there.
    let pinned = spawn_configurations::SpawnConfiguration {
        id: "launch/codex-home".into(),
        name: "Home".into(),
        spawn_option_id: "codex".into(),
        login_id: Some("home".into()),
        ..Default::default()
    };
    prefs.spawn_configurations.push(pinned.clone());
    let picked =
        spawn_configurations::with_login_pick(&prefs, "codex", Some(pinned.clone()), Some("work"))
            .unwrap()
            .unwrap();
    assert_eq!(picked.login_id.as_deref(), Some("work"));
    assert!(
        picked.resolved.is_none(),
        "node creation resolves the edited copy"
    );

    // No pick leaves the configuration exactly as saved.
    let untouched =
        spawn_configurations::with_login_pick(&prefs, "codex", Some(pinned.clone()), None)
            .unwrap()
            .unwrap();
    assert_eq!(untouched, pinned);

    // A pick of an unknown login is refused.
    assert!(spawn_configurations::with_login_pick(&prefs, "codex", None, Some("gone")).is_err());
}

#[test]
fn a_resumed_snapshot_keeps_its_frozen_login() {
    let prefs = prefs_with(vec![login("work", "Work")]);
    let mut plan = launch_configurations::capture(&prefs, "codex", &Default::default()).unwrap();
    launch_configurations::apply_login_pick(&prefs, &mut plan, Some("work")).unwrap();
    let resolved = launch_configurations::resolve_snapshot(&plan, &Default::default()).unwrap();
    assert_eq!(resolved.harness_login, plan.harness_login);
    assert!(resolved.harness_login.is_some());
}

#[test]
fn additional_homes_lists_only_this_harness() {
    let mut other = login("x", "X");
    other.harness_id = "claude".into();
    let prefs = prefs_with(vec![login("work", "Work"), other]);
    let homes = additional_homes(&prefs, "codex");
    assert_eq!(homes.len(), 1);
    assert!(homes[0].ends_with("work"));
}
