use super::*;
use crate::env::test_helpers::{init_repo_with_commit, TestDir};

fn fixture(name: &str) -> (TestDir, Repository, PathBuf, Config) {
    let td = TestDir::new(name);
    let source = init_repo_with_commit(td.path(), &[("README.md", "source")]);
    let checkout = td.path().join(".worktrees/checkout");
    super::super::create_git_worktree(
        td.path().to_str().unwrap(),
        checkout.to_str().unwrap(),
        "checkout",
        "detached",
        "HEAD",
    )
    .unwrap();
    let config = Config::open(&td.path().join("test-global-config")).unwrap();
    (td, source, checkout, config)
}

fn values(config: &Config, name: &str) -> Vec<String> {
    let mut entries = match config.multivar(name, None) {
        Ok(entries) => entries,
        Err(error) if error.code() == ErrorCode::NotFound => return Vec::new(),
        Err(error) => panic!("{error}"),
    };
    let mut values = Vec::new();
    while let Some(entry) = entries.next() {
        values.push(entry.unwrap().value().unwrap().to_string());
    }
    values
}

#[test]
fn authorization_adds_only_the_verified_exact_path_and_preserves_resets() {
    let (_td, source, checkout, mut config) = fixture("trust-exact");
    config.set_multivar(SAFE, "^$", "").unwrap();
    config
        .set_multivar(SAFE, "^unrelated$", "unrelated")
        .unwrap();
    config.set_str("user.name", "Keep this identity").unwrap();
    let key = authorize(&source, &checkout, &mut config).unwrap();
    assert_eq!(
        values(&config, SAFE),
        vec!["".to_string(), "unrelated".to_string(), key.clone()]
    );
    assert_eq!(values(&config, OWNED), vec![key.clone()]);
    remove_owned(&mut config, &key).unwrap();
    assert_eq!(values(&config, SAFE), vec!["", "unrelated"]);
    assert!(values(&config, OWNED).is_empty());
    assert_eq!(
        config.get_string("user.name").unwrap(),
        "Keep this identity"
    );
}

#[test]
fn foreign_and_unregistered_checkouts_gain_no_trust() {
    let (td, source, checkout, mut config) = fixture("trust-foreign");
    let foreign = TestDir::new("foreign-source");
    let foreign_source = init_repo_with_commit(foreign.path(), &[("README.md", "foreign")]);
    assert!(authorize(&foreign_source, &checkout, &mut config).is_err());
    let unrelated = td.path().join("unrelated");
    std::fs::create_dir(&unrelated).unwrap();
    init_repo_with_commit(&unrelated, &[("README.md", "unrelated")]);
    assert!(authorize(&source, &unrelated, &mut config).is_err());
    assert!(values(&config, SAFE).is_empty());
    assert!(values(&config, OWNED).is_empty());
}

#[test]
fn forged_common_directory_or_backpointer_cannot_acquire_trust() {
    for field in ["commondir", "gitdir"] {
        let (td, source, checkout, mut config) = fixture(&format!("trust-forged-{field}"));
        let admin = Repository::open(&checkout).unwrap().path().to_path_buf();
        let wrong = if field == "commondir" {
            td.path().to_path_buf()
        } else {
            td.path().join("README.md")
        };
        std::fs::write(admin.join(field), wrong.to_str().unwrap()).unwrap();
        assert!(authorize(&source, &checkout, &mut config).is_err());
        assert!(values(&config, SAFE).is_empty());
        assert!(values(&config, OWNED).is_empty());
    }
}

#[test]
fn a_linked_source_uses_its_common_repository_registry() {
    let (td, _source, checkout, mut config) = fixture("trust-linked-source");
    let sibling = td.path().join(".worktrees/sibling");
    super::super::create_git_worktree(
        td.path().to_str().unwrap(),
        sibling.to_str().unwrap(),
        "sibling",
        "detached",
        "HEAD",
    )
    .unwrap();
    let linked_source = Repository::open(&sibling).unwrap();
    let key = authorize(&linked_source, &checkout, &mut config).unwrap();
    assert_eq!(values(&config, SAFE), vec![key]);
}

#[test]
fn cleanup_preserves_preexisting_user_exceptions() {
    let (_td, _source, checkout, mut config) = fixture("trust-manual");
    let key = key(checkout.to_str().unwrap()).unwrap();
    config.set_multivar(SAFE, &exact(&key), &key).unwrap();
    remove_owned(&mut config, &key).unwrap();
    assert_eq!(values(&config, SAFE), vec![key]);
    assert!(values(&config, OWNED).is_empty());
}

#[test]
fn an_ineffective_user_exception_is_not_silently_overridden_or_claimed() {
    let (_td, source, checkout, mut config) = fixture("trust-overridden");
    let key = key(checkout.to_str().unwrap()).unwrap();
    config.set_multivar(SAFE, &exact(&key), &key).unwrap();
    config.set_multivar(SAFE, "^$", "").unwrap();
    assert!(authorize(&source, &checkout, &mut config)
        .unwrap_err()
        .contains("overrides"));
    assert_eq!(values(&config, SAFE), vec![key, "".to_string()]);
    assert!(values(&config, OWNED).is_empty());
}

#[test]
fn config_write_failures_are_errors() {
    let (_td, source, checkout, _config) = fixture("trust-write-failure");
    let mut unwritable = Config::new().unwrap();
    assert!(authorize(&source, &checkout, &mut unwritable)
        .unwrap_err()
        .contains("Cannot register Git trust"));
}

#[test]
fn managed_writes_stay_in_global_when_xdg_appears() {
    let (td, source, checkout, _config) = fixture("trust-global-level");
    let global_path = td.path().join("managed-global");
    let xdg_path = td.path().join("user-xdg");
    let mut global = Config::open(&global_path).unwrap();
    let mut protected = Config::new().unwrap();
    protected
        .add_file(&global_path, git2::ConfigLevel::Global, false)
        .unwrap();
    let key = authorize(&source, &checkout, &mut protected).unwrap();
    let mut xdg = Config::open(&xdg_path).unwrap();
    xdg.set_str("user.name", "Keep XDG identity").unwrap();
    xdg.set_multivar(SAFE, "^$", "unrelated-xdg").unwrap();
    protected
        .add_file(&xdg_path, git2::ConfigLevel::XDG, false)
        .unwrap();
    let next = format!("{key}-next");
    register(&mut protected, &next).unwrap();
    assert_eq!(values(&global, SAFE), vec![key.clone(), next.clone()]);
    assert_eq!(values(&global, OWNED), vec![key.clone(), next.clone()]);
    assert!(values(&xdg, OWNED).is_empty());
    remove_owned(&mut global, &key).unwrap();
    remove_owned(&mut global, &next).unwrap();
    assert!(values(&global, SAFE).is_empty());
    assert!(values(&global, OWNED).is_empty());
    assert_eq!(values(&xdg, SAFE), vec!["unrelated-xdg"]);
    assert_eq!(xdg.get_string("user.name").unwrap(), "Keep XDG identity");
}

#[test]
fn ineffective_xdg_user_exceptions_are_preserved_and_unclaimed() {
    let (td, source, checkout, _config) = fixture("trust-xdg-user");
    let global_path = td.path().join("managed-global");
    let xdg_path = td.path().join("user-xdg");
    let mut global = Config::open(&global_path).unwrap();
    global.set_multivar(SAFE, "^$", "").unwrap();
    let mut xdg = Config::open(&xdg_path).unwrap();
    let key = key(checkout.to_str().unwrap()).unwrap();
    xdg.set_multivar(SAFE, &exact(&key), &key).unwrap();
    let before = std::fs::read_to_string(&xdg_path).unwrap();
    let mut protected = Config::new().unwrap();
    protected
        .add_file(&global_path, git2::ConfigLevel::Global, false)
        .unwrap();
    protected
        .add_file(&xdg_path, git2::ConfigLevel::XDG, false)
        .unwrap();
    assert!(authorize(&source, &checkout, &mut protected)
        .unwrap_err()
        .contains("overrides"));
    assert_eq!(values(&global, SAFE), vec![""]);
    assert!(values(&global, OWNED).is_empty());
    assert!(values(&xdg, OWNED).is_empty());
    remove_owned(&mut global, &key).unwrap();
    assert_eq!(std::fs::read_to_string(&xdg_path).unwrap(), before);
}

#[test]
fn cleanup_uses_an_exact_value_even_with_regex_metacharacters() {
    let (td, _source, _checkout, mut config) = fixture("trust-regex");
    let target = td.path().join("a[1]+(b)");
    std::fs::create_dir(&target).unwrap();
    let key = key(target.to_str().unwrap()).unwrap();
    register(&mut config, &key).unwrap();
    let other = format!("{key}-other");
    register(&mut config, &other).unwrap();
    remove_owned(&mut config, &key).unwrap();
    assert_eq!(values(&config, SAFE), vec![other.clone()]);
    assert_eq!(values(&config, OWNED), vec![other]);
}

#[test]
fn removed_checkouts_keep_the_same_cleanup_key() {
    let (_td, _source, checkout, _config) = fixture("trust-removed-key");
    let before = key(checkout.to_str().unwrap()).unwrap();
    super::super::remove_one_worktree(checkout.to_str().unwrap()).unwrap();
    assert_eq!(key(checkout.to_str().unwrap()).unwrap(), before);
}

#[test]
fn wildcard_and_reset_values_cannot_be_registered() {
    let (_td, _source, _checkout, mut config) = fixture("trust-no-wildcards");
    for value in ["", "*", "/tmp/parent/*"] {
        assert!(register(&mut config, value).is_err());
    }
    assert!(values(&config, SAFE).is_empty());
    assert!(values(&config, OWNED).is_empty());
}
