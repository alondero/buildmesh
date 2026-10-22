use super::*;
use crate::env::test_helpers::init_repo_with_commit;

fn values(path: &Path, key: &str) -> Vec<String> {
    let config = Config::open(path).unwrap();
    let mut entries = match config.multivar(key, None) {
        Ok(entries) => entries,
        Err(error) if error.code() == ErrorCode::NotFound => return Vec::new(),
        Err(error) => panic!("{error}"),
    };
    let mut result = Vec::new();
    while let Some(entry) = entries.next() {
        result.push(entry.unwrap().value().unwrap().to_string());
    }
    result
}

fn fixture_worktree(root: &Path, name: &str) -> PathBuf {
    init_repo_with_commit(root, &[("file.txt", "initial\n")]);
    let target = root.join(".worktrees").join(name);
    crate::git::worktree::create_git_worktree(
        root.to_str().unwrap(),
        target.to_str().unwrap(),
        name,
        "detached",
        "HEAD",
    )
    .unwrap();
    target
}

#[test]
fn linkage_proof_rejects_unregistered_repositories_and_wrong_backpointers() {
    let root = tempfile::tempdir().unwrap();
    let target = fixture_worktree(root.path(), "original");
    let parent = Repository::open(root.path()).unwrap();
    verify_linkage(&parent, &target).unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    Repository::init(unrelated.path()).unwrap();
    assert_eq!(
        verify_linkage(&parent, unrelated.path())
            .unwrap_err()
            .code(),
        ErrorCode::Owner
    );
    let admin = Repository::open(&target).unwrap().path().to_path_buf();
    std::fs::write(
        admin.join("gitdir"),
        root.path().join(".git").to_string_lossy().as_bytes(),
    )
    .unwrap();
    assert_eq!(
        verify_linkage(&parent, &target).unwrap_err().code(),
        ErrorCode::Owner
    );
}

#[test]
fn linkage_proof_accepts_relative_links_and_retained_admin_names() {
    let root = tempfile::tempdir().unwrap();
    let target = fixture_worktree(root.path(), "original");
    let moved = root.path().join(".worktrees/renamed");
    crate::git::worktree::move_git_worktree(
        root.path().to_str().unwrap(),
        target.to_str().unwrap(),
        moved.to_str().unwrap(),
    )
    .unwrap();
    std::fs::write(
        moved.join(".git"),
        "gitdir: ../../.git/worktrees/original\n",
    )
    .unwrap();
    let parent = Repository::open(root.path()).unwrap();
    verify_linkage(&parent, &moved).unwrap();
}

#[test]
fn exact_config_values_preserve_user_entries_resets_and_regex_characters() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("trust.gitconfig");
    let mut config = Config::open(&file).unwrap();
    config.set_multivar("safe.directory", "^$", "").unwrap();
    set_exact(&mut config, "safe.directory", "C:/repo/[a].+$").unwrap();
    set_exact(&mut config, "safe.directory", "C:/repo/aaaZ").unwrap();
    set_exact(&mut config, "safe.directory", "C:/repo/[a].+$").unwrap();
    assert_eq!(
        values(&file, "safe.directory"),
        ["", "C:/repo/[a].+$", "C:/repo/aaaZ"]
    );
}

#[test]
fn aliases_preserve_distribution_and_case_sensitive_guest_paths() {
    let original = Path::new("//wsl$/Debian/home/Alice/Repo");
    let aliases = crate::env::wsl_host_path_aliases(original);
    assert_eq!(
        aliases,
        crate::env::wsl_host_path_aliases(Path::new("//wsl.localhost/Debian/home/Alice/Repo/"))
    );
    assert_ne!(
        aliases,
        crate::env::wsl_host_path_aliases(Path::new("//wsl$/Ubuntu/home/Alice/Repo"))
    );
    assert_ne!(
        aliases,
        crate::env::wsl_host_path_aliases(Path::new("//wsl$/Debian/home/alice/repo"))
    );
    for path in [
        "//wsl-runner/Debian/home/Alice/Repo",
        "//wsl.localhost/",
        "C:/repo",
    ] {
        assert!(crate::env::wsl_host_path_aliases(Path::new(path)).is_empty());
    }
}

// Search-path overrides belong to this child process alone. The same test
// executable also runs concurrently with unrelated Git tests in the parent.
fn isolate_config(dir: &Path) {
    let mut levels = vec![
        git2::ConfigLevel::Global,
        git2::ConfigLevel::XDG,
        git2::ConfigLevel::System,
    ];
    if cfg!(windows) {
        levels.push(git2::ConfigLevel::ProgramData);
    }
    for level in levels {
        unsafe {
            git2::opts::set_search_path(level, dir).unwrap();
        }
    }
}

#[test]
fn isolated_config_writer() {
    let Some(dir) = std::env::var_os("BUILDMESH_TRUST_TEST_CONFIG") else {
        return;
    };
    let dir = PathBuf::from(dir);
    isolate_config(&dir);
    let id = std::env::var("BUILDMESH_TRUST_TEST_WRITER").unwrap();
    for _ in 0..3 {
        record_grants(&dir.join(TRUST_FILE), &[format!("C:/fixture/[{id}].+$")]).unwrap();
    }
}

#[test]
fn concurrent_processes_preserve_exact_entries_and_a_single_include() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join(".gitconfig");
    std::fs::write(&global, "# user configuration\n[safe]\n\tdirectory = \n\tdirectory = C:/user-owned\n[include]\n\tpath = missing-user-include\n").unwrap();
    let mut children = Vec::new();
    for id in 0..4 {
        let dir = dir.path().to_path_buf();
        children.push(std::thread::spawn(move || {
            let mut command =
                crate::process_util::command_no_window(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "git::trust::tests::isolated_config_writer",
                    "--nocapture",
                ])
                .env("BUILDMESH_TRUST_TEST_CONFIG", dir)
                .env("BUILDMESH_TRUST_TEST_WRITER", id.to_string());
            crate::process_util::run_command_with_timeout(
                command,
                "isolated trust writer",
                Duration::from_secs(30),
            )
            .unwrap()
        }));
    }
    for child in children {
        let result = child.join().unwrap();
        assert!(
            result.status.success(),
            "{} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let mut grants = values(&dir.path().join(TRUST_FILE), "safe.directory");
    grants.sort();
    assert_eq!(
        grants,
        (0..4)
            .map(|id| format!("C:/fixture/[{id}].+$"))
            .collect::<Vec<_>>()
    );
    let includes = values(&global, "include.path");
    assert_eq!(
        includes.iter().filter(|s| s.ends_with(TRUST_FILE)).count(),
        1
    );
    assert!(includes.contains(&"missing-user-include".to_string()));
    let text = std::fs::read_to_string(global).unwrap();
    assert!(text.contains("# user configuration"));
    assert!(text.contains("C:/user-owned"));
}

#[cfg(windows)]
#[test]
#[ignore = "requires default WSL with Git and Windows Git; isolated child config"]
fn live_wsl_root_only_trust_provisions_reuses_moves_and_revokes() {
    let config = tempfile::tempdir().unwrap();
    let mut command = crate::process_util::command_no_window(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "git::trust::tests::live_wsl_trust_child",
            "--nocapture",
        ])
        .env("BUILDMESH_TRUST_LIVE_CONFIG", config.path())
        .env("GIT_CONFIG_GLOBAL", config.path().join(".gitconfig"))
        .env("GIT_CONFIG_SYSTEM", config.path().join("system.gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let output = crate::process_util::run_command_with_timeout(
        command,
        "WSL trust regression",
        Duration::from_secs(120),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
#[test]
fn live_wsl_trust_child() {
    let Some(config) = std::env::var_os("BUILDMESH_TRUST_LIVE_CONFIG") else {
        return;
    };
    let config = PathBuf::from(config);
    isolate_config(&config);
    let profile = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(profile.path().to_path_buf());
    let home = crate::env::wsl_home().expect("WSL home");
    let guest = tempfile::Builder::new()
        .prefix("buildmesh-trust-")
        .tempdir_in(crate::env::to_host_path(&home.to_string_lossy()))
        .unwrap();
    let root = guest.path();
    let root_path = crate::env::canonical_host_path_string(root);
    let mut global = Config::open(&config.join(".gitconfig")).unwrap();
    set_exact(&mut global, "safe.directory", &root_path).unwrap();
    init_repo_with_commit(root, &[("file.txt", "initial\n")]);
    let root_aliases = crate::env::wsl_host_path_aliases(root);
    for alias in &root_aliases {
        crate::git::project::resolve(alias)
            .expect("existing trusted root spelling resolves both aliases");
    }
    let manual = root.join(".worktrees/manual");
    let mut command = crate::process_util::git_command();
    command
        .arg("-C")
        .arg(root)
        .args(["worktree", "add", "--detach"])
        .arg(&manual)
        .arg("HEAD");
    let output = crate::process_util::run_command_with_timeout(
        command,
        "fixture worktree",
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parent = Repository::open(root).unwrap();
    assert_eq!(
        Repository::open(&manual).err().unwrap().code(),
        ErrorCode::Owner
    );
    global
        .remove_multivar(
            "safe.directory",
            &format!("^{}$", regex::escape(&root_path)),
        )
        .unwrap();
    assert_eq!(
        ensure_linked_worktree(&parent, &manual).unwrap_err().code(),
        ErrorCode::Owner
    );
    assert!(!profile.path().join(TRUST_FILE).exists());
    set_exact(&mut global, "safe.directory", &root_path).unwrap();
    let admin = parent.commondir().join("worktrees/manual");
    let back = std::fs::read(admin.join("gitdir")).unwrap();
    std::fs::write(
        admin.join("gitdir"),
        root.join(".git").to_string_lossy().as_bytes(),
    )
    .unwrap();
    assert!(ensure_linked_worktree(&parent, &manual).is_err());
    assert!(!profile.path().join(TRUST_FILE).exists());
    std::fs::write(admin.join("gitdir"), back).unwrap();
    // Reuse a registered legacy worktree with root-only trust, keeping its data.
    std::fs::write(manual.join("local.txt"), "keep me").unwrap();
    crate::git::worktree::provision::upgrade_warm_to_mode(
        &root_path,
        manual.to_str().unwrap(),
        "legacy-manual",
        "branched",
    )
    .unwrap();
    crate::git::worktree::create_git_worktree(
        &root_path,
        manual.to_str().unwrap(),
        "manual",
        "detached",
        "HEAD",
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(manual.join("local.txt")).unwrap(),
        "keep me"
    );
    crate::git::worktree::remove_one_worktree(manual.to_str().unwrap()).unwrap();
    let target = root.join(".worktrees/cold");
    // The test trusts only the root. Production worktree creation must derive
    // the two candidate grants before any later cross-runtime preparation.
    crate::git::worktree::create_git_worktree(
        &root_path,
        target.to_str().unwrap(),
        "cold",
        "detached",
        "HEAD",
    )
    .unwrap();
    for alias in crate::env::wsl_host_path_aliases(&target) {
        assert!(Repository::open(&alias).unwrap().is_worktree());
        crate::git::worktree::create_git_worktree(&root_path, &alias, "cold", "detached", "HEAD")
            .unwrap();
    }
    let moved = root.join(".worktrees/renamed");
    let alternate_root = root_aliases
        .iter()
        .find(|path| *path != &root_path)
        .unwrap();
    crate::git::worktree::move_git_worktree(
        alternate_root,
        target.to_str().unwrap(),
        moved.to_str().unwrap(),
    )
    .unwrap();
    crate::git::worktree::reset_warm_worktree(moved.to_str().unwrap(), "HEAD").unwrap();
    crate::git::worktree::prepare_cross_runtime_worktree(moved.to_str().unwrap()).unwrap();
    assert!(Repository::open(&moved).unwrap().is_worktree());
    let store = profile.path().join(TRUST_FILE);
    assert!(!values(&store, "safe.directory")
        .iter()
        .any(|p| p.ends_with("/cold")));
    let guest_path = crate::env::normalize_unc_to_wsl(moved.to_str().unwrap()).into_owned();
    let mut command = crate::process_util::command_no_window("wsl.exe");
    command.args([
        "-d",
        &crate::env::get_default_wsl_distro().unwrap(),
        "--exec",
        "git",
        "-C",
        &guest_path,
        "status",
        "--porcelain",
    ]);
    let output = crate::process_util::run_command_with_timeout(
        command,
        "guest Git trust regression",
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let guest_root = crate::env::normalize_unc_to_wsl(&root_path).into_owned();
    let mut command = crate::process_util::command_no_window("wsl.exe");
    command.args([
        "-d",
        &crate::env::get_default_wsl_distro().unwrap(),
        "--exec",
        "git",
        "-C",
        &guest_root,
        "worktree",
        "prune",
        "--expire",
        "now",
    ]);
    let output = crate::process_util::run_command_with_timeout(
        command,
        "guest prune protection",
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::read_to_string(Repository::open(&moved).unwrap().path().join("locked"))
            .unwrap()
            .contains("Buildmesh")
    );
    let mut command = crate::process_util::command_no_window(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "git::trust::tests::live_wsl_restart_child",
            "--nocapture",
        ])
        .env("BUILDMESH_TRUST_REOPEN", &moved);
    let output = crate::process_util::run_command_with_timeout(
        command,
        "restart trust regression",
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // A duplicate user-owned entry must survive revocation of our grant.
    let moved_path = crate::env::canonical_host_path_string(&moved);
    // A trusted linked checkout can itself back a Mesh even when its main
    // working directory is not independently trusted.
    global
        .remove_multivar(
            "safe.directory",
            &format!("^{}$", regex::escape(&root_path)),
        )
        .unwrap();
    assert_eq!(
        Repository::open(root).err().unwrap().code(),
        ErrorCode::Owner
    );
    let nested = root.join(".worktrees/from-linked-source");
    crate::git::worktree::create_git_worktree(
        &moved_path,
        nested.to_str().unwrap(),
        "from-linked-source",
        "detached",
        "HEAD",
    )
    .unwrap();
    assert!(Repository::open(&nested).unwrap().is_worktree());
    set_exact(&mut global, "safe.directory", &root_path).unwrap();
    crate::git::worktree::remove_one_worktree(nested.to_str().unwrap()).unwrap();
    set_exact(&mut global, "safe.directory", &moved_path).unwrap();
    crate::git::worktree::remove_one_worktree(moved.to_str().unwrap()).unwrap();
    assert!(!moved.exists());
    assert!(!values(&store, "safe.directory")
        .iter()
        .any(|p| p.ends_with("/renamed")));
    assert!(values(&config.join(".gitconfig"), "safe.directory").contains(&root_path));
    assert!(values(&config.join(".gitconfig"), "safe.directory").contains(&moved_path));

    // A denied profile/config write keeps a newly checked-out tree recoverable.
    crate::preferences::init_for_tests(config.join(".gitconfig"));
    let retained = root.join(".worktrees/retained");
    assert!(crate::git::worktree::create_git_worktree(
        &root_path,
        retained.to_str().unwrap(),
        "retained",
        "detached",
        "HEAD"
    )
    .is_err());
    assert!(retained.join(".git").is_file());
    std::fs::write(retained.join("local.txt"), "retained work").unwrap();
    let error = crate::git::worktree::create_git_worktree(
        &root_path,
        retained.to_str().unwrap(),
        "retained",
        "detached",
        "HEAD",
    )
    .unwrap_err();
    assert!(!error.contains("move the directory"), "{error}");
    assert!(!error.contains("non-worktree directory"), "{error}");
    crate::preferences::init_for_tests(profile.path().to_path_buf());
    crate::git::worktree::create_git_worktree(
        &root_path,
        retained.to_str().unwrap(),
        "retained",
        "detached",
        "HEAD",
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(retained.join("local.txt")).unwrap(),
        "retained work"
    );
    crate::git::worktree::remove_one_worktree(retained.to_str().unwrap()).unwrap();
}

#[cfg(windows)]
#[test]
fn live_wsl_restart_child() {
    let Some(path) = std::env::var_os("BUILDMESH_TRUST_REOPEN") else {
        return;
    };
    let config = std::env::var_os("BUILDMESH_TRUST_LIVE_CONFIG").unwrap();
    isolate_config(Path::new(&config));
    // No profile initialization or grant helper: a fresh process must load
    // persistent trust through ordinary protected Git configuration.
    assert!(Repository::open(path).unwrap().is_worktree());
}
