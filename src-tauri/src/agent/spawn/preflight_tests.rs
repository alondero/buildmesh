//! Tests for the provider-binary preflight (issue #823).
//!
//! The filesystem probe is injected, so both branches are covered without
//! depending on what is installed on the machine running the suite — and
//! without encoding a platform assumption, which is what broke the non-Windows
//! CI run on the first draft of this PR.

use super::preflight::{check_spawn_binary, ensure_spawn_binary_with, Preflight};
use crate::agent::launch_routing::PreparedLaunchRouting;
use crate::models::EnvType;
use crate::models::Provider;
use std::path::{Path, PathBuf};

fn no_file(_path: &Path) -> bool {
    false
}

fn no_stem(_stem: &str) -> Option<PathBuf> {
    None
}

fn missing_message(preflight: Preflight) -> String {
    match preflight {
        Preflight::Missing(message) => message,
        Preflight::Ready => panic!("expected a missing-binary verdict, got Ready"),
    }
}

#[test]
fn resolved_override_launches() {
    let preflight = check_spawn_binary(
        Provider::Anthropic,
        Some(Path::new(r"C:\npm\claude.cmd")),
        "claude",
        &|_path| true,
        &no_stem,
    );
    assert!(matches!(preflight, Preflight::Ready));
}

#[test]
fn stale_override_reports_the_missing_file() {
    let preflight = check_spawn_binary(
        Provider::Anthropic,
        Some(Path::new(r"C:\npm\claude.cmd")),
        "claude",
        &no_file,
        &no_stem,
    );
    let message = missing_message(preflight);
    assert!(
        message.contains(r"C:\npm\claude.cmd"),
        "the message must name the path it resolved: {message}"
    );
    assert!(
        message.contains("Anthropic (Claude)"),
        "the message must name the harness the user clicked: {message}"
    );
    assert!(
        message.contains("Reinstall"),
        "the remediation must be actionable: {message}"
    );
}

/// The override wins over the stem: a routing that resolved an absolute path
/// dispatches that file, so a live stem elsewhere must not mask its absence.
#[test]
fn stale_override_is_reported_even_when_the_stem_resolves() {
    let preflight = check_spawn_binary(
        Provider::Anthropic,
        Some(Path::new("/gone/claude")),
        "claude",
        &no_file,
        &|_stem| Some(PathBuf::from("/somewhere/else/claude")),
    );
    let message = missing_message(preflight);
    assert!(message.contains("/gone/claude"), "{message}");
}

/// `check_spawn_binary` must resolve exactly the stem its caller handed it —
/// the derivation is the caller's job, and a mismatch here would mean the
/// preflight validated a different program than the spawn dispatches.
#[test]
fn check_resolves_the_stem_it_is_given() {
    let probed = std::cell::RefCell::new(String::new());
    let verdict = check_spawn_binary(
        Provider::Anthropic,
        None,
        "sentinel-cli-no-install-produces-this",
        &no_file,
        &|stem| {
            *probed.borrow_mut() = stem.to_string();
            None
        },
    );
    assert_eq!(*probed.borrow(), "sentinel-cli-no-install-produces-this");
    let message = missing_message(verdict);
    assert!(
        message.contains("sentinel-cli-no-install-produces-this"),
        "the miss must name the stem that was probed: {message}"
    );
}

#[test]
fn bare_stem_resolving_through_the_enriched_path_launches() {
    let preflight = check_spawn_binary(Provider::Anthropic, None, "claude", &no_file, &|_stem| {
        Some(PathBuf::from("/opt/homebrew/bin/claude"))
    });
    assert!(matches!(preflight, Preflight::Ready));
}

#[test]
fn unresolvable_bare_stem_names_the_missing_command() {
    let preflight = check_spawn_binary(Provider::Anthropic, None, "claude", &no_file, &no_stem);
    let message = missing_message(preflight);
    assert!(
        message.contains('`') && message.contains("command wasn't found"),
        "the message must name the missing command: {message}"
    );
    assert!(
        message.contains("PATH"),
        "the message must point at the PATH as the thing to fix: {message}"
    );
}

/// Each adapter's own stem is the one probed, so a harness with a non-obvious
/// binary name reports its real name rather than a generic "agent".
///
/// The expected stems are **derived from the adapters, not hardcoded**: the
/// recipe binary is platform-dependent (Claude Code is `claude.exe` on Windows
/// and `claude` everywhere else), so a literal table made this test fail on the
/// Linux and macOS CI runners. Deriving them keeps the assertion meaningful —
/// it still fails if `ensure_spawn_binary` probes some other name — without
/// encoding a platform assumption. `host_native_env_type` is
/// `EnvType::Windows` on a Windows host, which is what `recipe_binary_for`
/// maps onto `Platform::current()`.
#[test]
fn the_probed_stem_is_the_adapter_recipe_binary() {
    for provider in [
        Provider::Anthropic,
        Provider::Codex,
        Provider::OpenCode,
        Provider::Cline,
        Provider::Kimi,
    ] {
        let expected = crate::agent::launch_routing::recipe_binary_for(provider, EnvType::Windows);
        let probed = std::cell::RefCell::new(String::new());
        let _ = ensure_spawn_binary_with(
            provider,
            EnvType::Windows,
            &PreparedLaunchRouting::Native { executable: None },
            &no_file,
            &|stem| {
                *probed.borrow_mut() = stem.to_string();
                None
            },
        );
        assert_eq!(
            *probed.borrow(),
            expected,
            "wrong stem probed for {provider:?}"
        );
    }
}

/// Guards the platform variance that broke the Linux CI run: Claude Code's
/// recipe binary carries the `.exe` suffix only on Windows, so the stem this
/// module probes genuinely differs per platform. A literal stem table is what
/// made the suite fail on `ubuntu-latest` (review finding 1 on PR #2160).
#[test]
fn the_anthropic_stem_carries_the_exe_suffix_only_on_windows() {
    let stem =
        crate::agent::launch_routing::recipe_binary_for(Provider::Anthropic, EnvType::Windows);
    assert_eq!(
        stem,
        if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        }
    );
}

#[test]
fn terminal_is_exempt_because_a_shell_is_never_uninstalled() {
    // No override and nothing resolvable: a miss message would be
    // unactionable, so the preflight must not produce one. The exemption
    // lives in `ensure_spawn_binary` (it needs the real provider), which is
    // why this asserts through the entry point rather than the inner helper.
    assert!(matches!(
        super::preflight::ensure_spawn_binary(
            Provider::Terminal,
            EnvType::Windows,
            &PreparedLaunchRouting::Native { executable: None },
        ),
        Preflight::Ready
    ));
}

/// A routing can pin its own runtime (the Codex proxy picks an install during
/// prepare). The preflight must honour it rather than assume the mesh path's.
#[test]
fn a_pinned_runtime_is_honoured_over_the_mesh_runtime() {
    let routing = PreparedLaunchRouting::CodexProxy {
        harness_id: "codex".into(),
        provider_id: "minimax".into(),
        profile_name: "Codex".into(),
        descriptor: codex_verification_descriptor(),
        verification: crate::preferences::PairingVerification {
            harness_id: "codex".into(),
            provider_id: "minimax".into(),
            pairing_signature: "sig".into(),
            endpoint: "https://example.invalid".into(),
            model_id: "model".into(),
            auth_mode: crate::agent::provider::compatibility::ProviderAuthMode::BearerEnv,
            runtime: "wsl".into(),
            executable: "/home/dev/.npm-global/bin/codex".into(),
            codex_version: "0.144.0".into(),
            capability_result: Default::default(),
            status: crate::preferences::PairingVerificationStatus::Verified,
            verified_at: None,
            reason: None,
        },
        runtime: EnvType::Wsl,
        install: crate::agent::provider::adapters::codex::CodexInstall {
            executable: "/home/dev/.npm-global/bin/codex".into(),
            version: "0.1.0".into(),
            runtime_identity: "wsl".into(),
            codex_home: "/home/dev/.codex".into(),
            wsl_distro: Some("Ubuntu".into()),
        },
        credential_reference: "ref".into(),
        credential: "secret".into(),
    };

    // The guest path does not exist on the Windows host, so a host-side
    // existence check would wrongly report a miss. The pinned WSL runtime
    // must exempt the preflight instead.
    assert_eq!(routing.pinned_runtime(), Some(EnvType::Wsl));
    assert!(matches!(
        super::preflight::ensure_spawn_binary(Provider::Codex, EnvType::Windows, &routing),
        Preflight::Ready
    ));
}

/// `Native` / `Environment` inherit the mesh path's runtime, so they must not
/// pin one — otherwise a WSL mesh would skip the preflight for every harness.
#[test]
fn native_and_environment_routings_pin_no_runtime() {
    assert_eq!(
        PreparedLaunchRouting::Native { executable: None }.pinned_runtime(),
        None
    );
    assert_eq!(
        PreparedLaunchRouting::Environment {
            values: Vec::new(),
            executable: None
        }
        .pinned_runtime(),
        None
    );
}

/// A WSL mesh spawn resolves the stem inside the guest login shell. Probing
/// it on the host would read a false miss and block a working spawn.
#[test]
fn guest_mesh_runtimes_are_exempt() {
    for env_type in [EnvType::Wsl, EnvType::WindowsInterop] {
        assert!(
            matches!(
                super::preflight::ensure_spawn_binary(
                    Provider::Anthropic,
                    env_type,
                    &PreparedLaunchRouting::Native { executable: None },
                ),
                Preflight::Ready
            ),
            "{env_type:?} must be exempt from host-side preflight"
        );
    }
}

/// A host-native routing whose resolved override no longer exists on disk must
/// refuse before the PTY opens.
///
/// This exercises the **override** branch through the real entry point: the
/// routing carries an absolute path, so `wrap` would dispatch that exact file.
/// (Review finding 3 on PR #2160: this test previously carried a comment
/// claiming it covered the unresolvable bare-stem search, which it never
/// reached. The bare-stem branch is covered by
/// `host_native_unresolvable_bare_stem_refuses_the_spawn` below.)
#[test]
fn host_native_missing_override_path_refuses_the_spawn() {
    let routing = PreparedLaunchRouting::Native {
        executable: Some(PathBuf::from(r"C:\definitely\not\here\missing-cli.exe")),
    };
    assert!(matches!(
        super::preflight::ensure_spawn_binary(Provider::Codex, EnvType::Windows, &routing),
        Preflight::Missing(_)
    ));
}

/// The other host-native branch: no override at all, so `wrap` falls back to
/// the bare recipe stem and the enriched search cannot find it.
///
/// Driving this through `ensure_spawn_binary_with` with an injected resolver is
/// what makes it a real test — depending on the machine's installed CLIs (or on
/// a stem literal) is exactly what made the previous version of this file fail
/// on non-Windows CI.
#[test]
fn host_native_unresolvable_bare_stem_refuses_the_spawn() {
    let verdict = ensure_spawn_binary_with(
        Provider::Anthropic,
        EnvType::Windows,
        &PreparedLaunchRouting::Native { executable: None },
        &no_file,
        &no_stem,
    );
    let message = missing_message(verdict);
    assert!(
        message.contains("command wasn't found"),
        "the bare-stem miss must name the missing command: {message}"
    );
    assert!(
        message.contains("Anthropic (Claude)"),
        "the message must name the harness the user clicked: {message}"
    );
}

/// The bare stem resolving through the enriched search must NOT block the
/// spawn — the preflight only rejects what a shell lookup would also miss.
#[test]
fn host_native_resolvable_bare_stem_lets_the_spawn_proceed() {
    let verdict = ensure_spawn_binary_with(
        Provider::Anthropic,
        EnvType::Windows,
        &PreparedLaunchRouting::Native { executable: None },
        &no_file,
        &|_stem| Some(PathBuf::from("/opt/homebrew/bin/claude")),
    );
    assert!(matches!(verdict, Preflight::Ready));
}

fn codex_verification_descriptor() -> crate::agent::provider::compatibility::EndpointModelDescriptor
{
    crate::agent::provider::compatibility::EndpointModelDescriptor {
        provider_id: "minimax".into(),
        endpoint: "https://example.invalid".into(),
        model_id: "model".into(),
        ..Default::default()
    }
}
