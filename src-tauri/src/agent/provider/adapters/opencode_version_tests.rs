use super::*;

fn fixture(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fixture.cjs");
    std::fs::write(&script, source).unwrap();
    let executable = dir.path().join(if cfg!(windows) {
        "opencode.cmd"
    } else {
        "opencode"
    });
    let wrapper = if cfg!(windows) {
        format!("@echo off\r\nnode \"{}\" %*\r\n", script.display())
    } else {
        format!("#!/bin/sh\nexec node '{}' \"$@\"\n", script.display())
    };
    std::fs::write(&executable, wrapper).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    (dir, executable)
}

fn fixture_command(executable: &Path) -> std::process::Command {
    let recipe = SpawnRecipe {
        binary: "opencode",
        base_args: vec![],
        trailing_args: vec![],
        windows_shell: shell_for(Platform::current()),
    };
    crate::agent::spawn_environment::background_command(&recipe, Some(executable)).unwrap()
}

#[test]
fn private_commands_drain_large_stdout_and_stderr_before_exit() {
    let (_dir, executable) = fixture(
        "process.stdout.write('x'.repeat(1048576)); process.stderr.write('y'.repeat(1048576));",
    );
    let output = run_private_command(
        fixture_command(&executable),
        "large reply",
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, vec![b'x'; 1048576]);
    assert_eq!(output.stderr, vec![b'y'; 1048576]);
}

#[test]
fn probe_cache_reuses_results_and_invalidates_a_changed_executable() {
    let (dir, executable) =
        fixture("require('fs').appendFileSync(__dirname+'/count','x'); console.log('2.2.3');");
    assert_eq!(probe(&executable), Some(2));
    assert_eq!(probe(&executable), Some(2));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("count")).unwrap(),
        "x"
    );
    std::fs::write(
        dir.path().join("fixture.cjs"),
        "require('fs').appendFileSync(__dirname+'/count','x'); console.log('1.18.3');",
    )
    .unwrap();
    let modified = executable.metadata().unwrap().modified().unwrap();
    std::fs::File::options()
        .write(true)
        .open(&executable)
        .unwrap()
        .set_modified(modified + Duration::from_secs(2))
        .unwrap();
    assert_eq!(probe(&executable), Some(1));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("count")).unwrap(),
        "xx"
    );
}

#[test]
fn v1_interactive_and_background_launches_allow_unknown_and_timed_out_probes() {
    use crate::agent::{
        capabilities::ResolvedAgentConfig,
        launch::SessionIdModeRef,
        provider::{AgentProvider, SessionPreparation},
    };
    use crate::preferences::launch_configurations::{capture, LaunchOverrides};
    for (source, timeout) in [
        (
            "console.log('unexpected version output');",
            Duration::from_secs(5),
        ),
        ("setTimeout(() => {}, 10000);", Duration::from_millis(150)),
    ] {
        let (dir, executable) = fixture(source);
        // Seed the same production cache with a genuinely unknown/timeout result.
        assert_eq!(probe_with_timeout(&executable, timeout), None);
        let path = dir.path().to_string_lossy().into_owned();
        let resolved = crate::env::ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: crate::models::EnvType::Windows,
        };
        assert!(super::super::opencode::OPENCODE
            .prepare_session(SessionPreparation {
                resolved: &resolved,
                executable: Some(&executable),
                node_id: 1,
                session: SessionIdModeRef::None,
                config: &ResolvedAgentConfig::default(),
                sandbox: false,
            })
            .is_ok());
        let prefs = crate::preferences::AppPreferences::default();
        let mut plan = capture(&prefs, "opencode", &LaunchOverrides::default()).unwrap();
        plan.harness.executable = Some(executable.clone());
        let launch = crate::agent::background::resolve_plan(plan, &prefs).unwrap();
        assert!(launch
            .command(dir.path(), &dir.path().join("result"), "hello")
            .is_ok());
        let error = super::super::opencode2::OPENCODE2
            .validate_executable(&executable)
            .unwrap_err();
        assert!(error.contains("Could not determine"), "{error}");
        assert!(error.contains("retry"), "{error}");
        assert!(!error.contains("Pin"), "{error}");
    }
}

#[test]
fn version_probe_removes_forced_color_from_the_child_environment() {
    let (_dir, executable) = fixture("console.log(process.env.FORCE_COLOR === undefined && process.env.NO_COLOR === '1' ? 'opencode v2.2.3' : 'unexpected probe environment');");
    let mut command = fixture_command(&executable);
    command
        .arg("--version")
        .env("FORCE_COLOR", "1")
        .env("NO_COLOR", "0");
    assert_eq!(run_version_probe(command, Duration::from_secs(5)), Some(2));
}

#[test]
fn force_color_version_output_identifies_v2() {
    assert_eq!(
        major("\x1b[1mopencode\x1b[0m \x1b[2mv\x1b[0m\x1b[1m2.2.3\x1b[0m\r\n"),
        Some(2)
    );
}

#[test]
fn versions_are_strict_and_shared_binary_majors_stay_distinct() {
    assert_eq!(major("opencode v2.2.3\r\n"), Some(2));
    assert_eq!(major("1.18.3\n"), Some(1));
    assert_eq!(major("v3.0.0"), Some(3));
    for malformed in ["", "2", "opencode unknown", "error 2.2.3", "2.x.3"] {
        assert_eq!(major(malformed), None);
    }
}
