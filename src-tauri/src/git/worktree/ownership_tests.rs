//! Ownership lifecycle coverage. Run the ignored live test alone: it redirects
//! process-wide libgit2 config discovery.

use super::*;
use crate::env::test_helpers::init_repo_with_commit;
#[cfg(windows)]
use crate::models::AgentNode;
#[cfg(windows)]
use provision::{NullSink, ProvisionHooks, ProvisionOutcome, SpawnContext, SpawnSource};

#[cfg(windows)]
fn safe_path(path: &std::path::Path) -> String {
    crate::env::canonical_host_path_string(&std::fs::canonicalize(path).unwrap()).replace('\\', "/")
}

#[cfg(windows)]
fn drop_value(config: &mut git2::Config, name: &str, path: &str) {
    match config.remove_multivar(name, &format!("^{}$", regex::escape(path))) {
        Ok(()) => {}
        Err(error) if error.code() == git2::ErrorCode::NotFound => {}
        Err(error) => panic!("{error}"),
    }
}

fn values(config: &git2::Config, name: &str) -> Vec<String> {
    let mut entries = match config.multivar(name, None) {
        Ok(entries) => entries,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Vec::new(),
        Err(error) => panic!("{error}"),
    };
    let mut values = Vec::new();
    while let Some(entry) = entries.next() {
        values.push(entry.unwrap().value().unwrap().to_string());
    }
    values
}

#[test]
fn native_owned_checkout_needs_no_global_exception() {
    let root = tempfile::tempdir().unwrap();
    init_repo_with_commit(root.path(), &[("README.md", "native ownership")]);
    let checkout = root.path().join(".worktrees/native-owned");
    create_git_worktree(
        root.path().to_str().unwrap(),
        checkout.to_str().unwrap(),
        "native-owned",
        "detached",
        "HEAD",
    )
    .unwrap();
    Repository::open(&checkout).unwrap();
    let key = trust::key(checkout.to_str().unwrap()).unwrap();
    let config = git2::Config::open_default().unwrap();
    let exceptions = values(&config, "safe.directory");
    let markers = values(&config, "buildmesh.managedWorktreeSafeDirectory");
    // Remove before asserting so a regression does not strand fixture trust.
    remove_one_worktree(checkout.to_str().unwrap()).unwrap();
    assert!(!exceptions.contains(&key));
    assert!(!markers.contains(&key));
}

#[cfg(windows)]
fn context(root: &std::path::Path, target: &std::path::Path) -> SpawnContext {
    SpawnContext {
        node: AgentNode {
            path: root.to_str().unwrap().to_string(),
            worktree_name: Some("issue-checkout".to_string()),
            use_worktree: true,
            ..Default::default()
        },
        source: SpawnSource::Issue,
        base_ref: "HEAD".to_string(),
        worktree_mode: "detached".to_string(),
        use_worktree: true,
        warm_entry: None,
        host_path: target.to_str().unwrap().to_string(),
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires Windows and WSL; run this exact test alone to isolate libgit2 config discovery"]
fn live_wsl_managed_worktree_trust() {
    let guest_home = crate::env::wsl_home().expect("WSL home");
    let root = tempfile::Builder::new()
        .prefix("buildmesh-trust-")
        .tempdir_in(crate::env::to_host_path(&guest_home.to_string_lossy()))
        .unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join(".gitconfig");
    std::fs::write(&config_path, "").unwrap();

    struct Restore(Vec<(git2::ConfigLevel, std::ffi::CString)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (level, path) in self.0.drain(..) {
                unsafe { git2::opts::set_search_path(level, path).unwrap() };
            }
        }
    }
    let levels = [
        git2::ConfigLevel::Global,
        git2::ConfigLevel::XDG,
        git2::ConfigLevel::System,
        git2::ConfigLevel::ProgramData,
    ];
    let _restore = Restore(
        levels
            .iter()
            .map(|level| {
                (*level, unsafe {
                    git2::opts::get_search_path(*level).unwrap()
                })
            })
            .collect(),
    );
    for level in levels {
        unsafe { git2::opts::set_search_path(level, config_dir.path()).unwrap() };
    }
    assert_eq!(
        std::fs::canonicalize(git2::Config::find_global().unwrap()).unwrap(),
        std::fs::canonicalize(&config_path).unwrap()
    );

    let mut config = git2::Config::open(&config_path).unwrap();
    let root_key = safe_path(root.path());
    config
        .set_multivar(
            "safe.directory",
            &format!("^{}$", regex::escape(&root_key)),
            &root_key,
        )
        .unwrap();
    init_repo_with_commit(root.path(), &[("README.md", "ownership fixture")]);
    drop_value(&mut config, "safe.directory", &root_key);
    let untrusted_config = std::fs::read_to_string(&config_path).unwrap();
    let pool = root.path().join(".worktrees/pool");
    let target = root.path().join(".worktrees/issue-checkout");
    assert_eq!(
        Repository::open(root.path()).err().unwrap().code(),
        git2::ErrorCode::Owner
    );
    assert!(create_git_worktree(
        root.path().to_str().unwrap(),
        pool.to_str().unwrap(),
        "pool",
        "detached",
        "HEAD"
    )
    .is_err());
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        untrusted_config
    );

    config
        .set_multivar(
            "safe.directory",
            &format!("^{}$", regex::escape(&root_key)),
            &root_key,
        )
        .unwrap();
    Repository::open(root.path()).unwrap();
    create_git_worktree(
        root.path().to_str().unwrap(),
        pool.to_str().unwrap(),
        "pool",
        "detached",
        "HEAD",
    )
    .unwrap();
    Repository::open(&pool).expect("a cold WSL worktree must inherit trust from its source");
    let pool_key = safe_path(&pool);

    // Adding XDG config after registration must not redirect managed cleanup.
    let xdg_path = config_dir.path().join("config");
    std::fs::write(&xdg_path, "[user]\n\tname = Keep XDG identity\n").unwrap();
    let xdg_before = std::fs::read_to_string(&xdg_path).unwrap();

    // A failed revocation must happen before the rename, so the fallback
    // cannot strand trust for a path that no longer holds this checkout.
    let config_lock = config_dir.path().join(".gitconfig.lock");
    std::fs::write(&config_lock, "held by another Git process").unwrap();
    let error = move_git_worktree(
        root.path().to_str().unwrap(),
        pool.to_str().unwrap(),
        target.to_str().unwrap(),
    )
    .unwrap_err();
    assert!(
        error.contains("Cannot revoke Git trust before moving"),
        "{error}"
    );
    assert!(pool.exists());
    assert!(!target.exists());
    assert!(values(&config, "safe.directory").contains(&pool_key));
    std::fs::remove_file(&config_lock).unwrap();

    // A Git-level refusal after revocation restores trust at the old path.
    let worktree_lock = Repository::open(&pool).unwrap().path().join("locked");
    std::fs::write(&worktree_lock, "fixture lock").unwrap();
    let error = move_git_worktree(
        root.path().to_str().unwrap(),
        pool.to_str().unwrap(),
        target.to_str().unwrap(),
    )
    .unwrap_err();
    assert!(error.contains("git worktree move failed"), "{error}");
    Repository::open(&pool).expect("a refused move must restore source trust");
    assert!(!target.exists());
    std::fs::remove_file(worktree_lock).unwrap();

    let mut warm = context(root.path(), &target);
    warm.warm_entry = Some(crate::services::warm_pool::ClaimedWarmEntry {
        id: 7,
        path: pool.to_str().unwrap().to_string(),
        preassigned_name: "pool".to_string(),
        base_sha: None,
    });
    assert!(matches!(
        provision::provision_for_spawn(warm, &ProvisionHooks::default(), &NullSink).unwrap(),
        ProvisionOutcome::Adopted { .. }
    ));
    Repository::open(&target)
        .expect("a renamed WSL worktree must inherit trust for its final path");
    let target_key = safe_path(&target);
    assert!(!values(&config, "safe.directory").contains(&pool_key));

    drop_value(&mut config, "safe.directory", &target_key);
    drop_value(
        &mut config,
        "buildmesh.managedWorktreeSafeDirectory",
        &target_key,
    );
    assert_eq!(
        Repository::open(&target).err().unwrap().code(),
        git2::ErrorCode::Owner
    );
    assert!(matches!(
        provision::provision_for_spawn(
            context(root.path(), &target),
            &ProvisionHooks::default(),
            &NullSink
        )
        .unwrap(),
        ProvisionOutcome::Reused { .. }
    ));
    Repository::open(&target).unwrap();
    drop_value(&mut config, "safe.directory", &target_key);
    drop_value(
        &mut config,
        "buildmesh.managedWorktreeSafeDirectory",
        &target_key,
    );
    let mut collision = context(root.path(), &target);
    collision.warm_entry = Some(crate::services::warm_pool::ClaimedWarmEntry {
        id: 9,
        path: pool.to_str().unwrap().to_string(),
        preassigned_name: "pool".to_string(),
        base_sha: None,
    });
    assert!(matches!(
        provision::provision_for_spawn(collision, &ProvisionHooks::default(), &NullSink).unwrap(),
        ProvisionOutcome::Reused { .. }
    ));
    Repository::open(&target).expect("warm fallback reuse must validate trust too");
    // Removal can finish on disk before config unlocks. Its detailed failure
    // keeps the existing pending-removal queue alive; retry the absent path.
    std::fs::write(&config_lock, "held by another Git process").unwrap();
    let failure = remove_one_worktree_and_branch_detailed(target.to_str().unwrap()).unwrap_err();
    assert_eq!(failure.operation, OP_REVOKE_TRUST);
    assert!(!target.exists());
    assert!(values(&config, "safe.directory").contains(&target_key));
    std::fs::remove_file(&config_lock).unwrap();
    remove_one_worktree(target.to_str().unwrap()).unwrap();
    assert!(!values(&config, "safe.directory").contains(&target_key));
    assert_eq!(values(&config, "safe.directory"), vec![root_key.clone()]);
    assert!(values(&config, "buildmesh.managedWorktreeSafeDirectory").is_empty());

    let manual = root.path().join(".worktrees/manual-pool");
    create_git_worktree(
        root.path().to_str().unwrap(),
        manual.to_str().unwrap(),
        "manual-pool",
        "detached",
        "HEAD",
    )
    .unwrap();
    let manual_key = safe_path(&manual);
    drop_value(&mut config, "safe.directory", &manual_key);
    drop_value(
        &mut config,
        "buildmesh.managedWorktreeSafeDirectory",
        &manual_key,
    );
    let mut warm_manual = context(root.path(), &manual);
    warm_manual.source = SpawnSource::Manual;
    warm_manual.node.worktree_name = Some("manual-pool".to_string());
    warm_manual.warm_entry = Some(crate::services::warm_pool::ClaimedWarmEntry {
        id: 8,
        path: manual.to_str().unwrap().to_string(),
        preassigned_name: "manual-pool".to_string(),
        base_sha: None,
    });
    assert!(matches!(
        provision::provision_for_spawn(warm_manual, &ProvisionHooks::default(), &NullSink).unwrap(),
        ProvisionOutcome::Upgraded { .. }
    ));
    Repository::open(&manual).unwrap();
    remove_one_worktree(manual.to_str().unwrap()).unwrap();
    assert_eq!(values(&config, "safe.directory"), vec![root_key.clone()]);
    assert!(values(&config, "buildmesh.managedWorktreeSafeDirectory").is_empty());
    assert_eq!(std::fs::read_to_string(&xdg_path).unwrap(), xdg_before);

    // An XDG exception reset by Global is user-owned even though ineffective.
    let user_checkout = root.path().join(".worktrees/user-owned");
    let user_key = trust::key(user_checkout.to_str().unwrap()).unwrap();
    let mut xdg = git2::Config::open(&xdg_path).unwrap();
    xdg.set_multivar("safe.directory", "^$", &user_key).unwrap();
    let xdg_before = std::fs::read_to_string(&xdg_path).unwrap();
    config.set_multivar("safe.directory", "^$", "").unwrap();
    config
        .set_multivar("safe.directory", "^missing-fixture-path$", &root_key)
        .unwrap();
    let error = create_git_worktree(
        root.path().to_str().unwrap(),
        user_checkout.to_str().unwrap(),
        "user-owned",
        "detached",
        "HEAD",
    )
    .unwrap_err();
    assert!(error.contains("overrides"), "{error}");
    assert!(!values(&config, "safe.directory").contains(&user_key));
    assert!(values(&config, "buildmesh.managedWorktreeSafeDirectory").is_empty());
    assert_eq!(std::fs::read_to_string(&xdg_path).unwrap(), xdg_before);
    drop_value(&mut config, "safe.directory", "");
    remove_one_worktree(user_checkout.to_str().unwrap()).unwrap();
    assert_eq!(std::fs::read_to_string(&xdg_path).unwrap(), xdg_before);
}
