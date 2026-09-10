//! Startup auto-detection of installed agent harnesses (PRD #534 / issue #536).
//!
//! On launch we scan the system `PATH` (plus a few standard config dirs) for the
//! CLI binaries that back our harnesses, and turn each *present* tool into a
//! dynamic [`HarnessProfile`]. Only detected executables become profiles, so an
//! absent tool (e.g. Codex on a machine that never installed it) never clutters
//! the launch menu.
//!
//! The scan is a dep-free, in-process `stat` sweep — no `which`/`where`
//! subprocess and no new crates — so its startup cost is a few hundred cached
//! metadata lookups (typically a couple of milliseconds). The pure
//! [`detect_profiles`] takes its filesystem probe as a closure so the
//! present-vs-absent logic is unit-testable without touching the real disk
//! (issue #536 AC5).

use crate::preferences::HarnessProfile;
use std::path::{Path, PathBuf};

/// A harness whose presence we can sniff at startup.
struct Detectable {
    /// Profile id written to `preferences.json` and the DB `provider` column.
    id: &'static str,
    /// Menu label shown in the launch dropdown.
    name: &'static str,
    /// Backing legacy [`crate::models::Provider`] id, resolved at spawn time by
    /// [`crate::preferences::resolve_harness_provider`].
    harness: &'static str,
    /// Binary stems to look for on `PATH` (platform extensions appended).
    binaries: &'static [&'static str],
    /// Home-relative config dirs that also count as "installed" — a tool whose
    /// binary isn't on `PATH` but whose config dir exists is still offered, so a
    /// shell-function or alias install still surfaces.
    config_dirs: &'static [&'static str],
}

/// The tools we auto-detect. Claude Code backs the `anthropic` harness; the
/// others map id-to-id onto their legacy [`crate::models::Provider`]. MiniMax
/// is a `claude`-with-env redirect (no binary of its own), so it's configured
/// manually rather than detected (PRD #534: custom compatible profiles are out
/// of scope for V1 auto-detection). Kimi Code (wayfinder #918) IS a native
/// binary on PATH as `kimi` and ships `~/.kimi/` for config — both count as
/// "installed" so a shell-function or alias install still surfaces.
const DETECTABLE: &[Detectable] = &[
    Detectable {
        id: "muse",
        name: "Meta Muse",
        harness: "muse",
        binaries: &["muse"],
        config_dirs: &[],
    },
    Detectable {
        id: "claude",
        name: "Claude Code",
        harness: "anthropic",
        binaries: &["claude"],
        config_dirs: &[".claude"],
    },
    Detectable {
        id: "codex",
        name: "Codex",
        harness: "codex",
        binaries: &["codex"],
        config_dirs: &[".codex"],
    },
    Detectable {
        id: "cursor",
        name: "Cursor Agent",
        harness: "cursor",
        // No bare "agent" stem: it false-positived against unrelated PATH
        // binaries of that name (e.g. Grok's agent.exe) and offered Cursor
        // on machines without it. Alias installs are still caught via .cursor.
        binaries: &["cursor-agent"],
        config_dirs: &[".cursor"],
    },
    Detectable {
        id: "agy",
        name: "Antigravity",
        harness: "agy",
        binaries: &["agy"],
        // Antigravity-only signals — issue #1287. `.gemini/antigravity-cli/`
        // is the CLI's canonical home (conversations, brain, log, bin);
        // `.antigravity/` is the IDE fork's home (extensions/, argv.json);
        // `.antigravitycli/` is the symlink layer some installs use to
        // mirror config under a shorter name. Deliberately NOT `.gemini/`:
        // that directory is shared with Google Gemini CLI (oauth_creds,
        // google_accounts, settings), so a bare `.gemini` entry would
        // false-positive Antigravity onto machines with only Gemini CLI
        // installed — the same class of bug the cursor-agent stem note
        // above documents.
        config_dirs: &[".gemini/antigravity-cli", ".antigravity", ".antigravitycli"],
    },
    Detectable {
        id: "opencode",
        name: "OpenCode",
        harness: "opencode",
        binaries: &["opencode"],
        config_dirs: &[],
    },
    Detectable {
        id: "grok",
        name: "Grok Code",
        harness: "grok",
        binaries: &["grok"],
        config_dirs: &[".grok"],
    },
    Detectable {
        id: "kimi",
        name: "Kimi Code",
        harness: "kimi",
        binaries: &["kimi"],
        config_dirs: &[".kimi"],
    },
    Detectable {
        id: "mcode",
        name: "MiniMax Code",
        harness: "mcode",
        binaries: &["mcode"],
        config_dirs: &[".mcode", ".minimax-code"],
    },
    Detectable {
        id: "dsh",
        name: "DeepSeek Harness",
        harness: "dsh",
        binaries: &["dsh"],
        config_dirs: &[".dsh", ".deepseek-harness"],
    },
    Detectable {
        id: "commandcode",
        name: "Command Code",
        harness: "commandcode",
        #[cfg(windows)]
        binaries: &["cmdc"],
        #[cfg(not(windows))]
        binaries: &["cmd"],
        config_dirs: &[".commandcode"],
    },
    Detectable {
        id: "freebuff",
        name: "Freebuff",
        harness: "freebuff",
        binaries: &["freebuff"],
        // Freebuff (issue #1437) is an interactive AI coding agent CLI built on
        // Codebuff. Global npm installs place `freebuff` (or `freebuff.cmd` on
        // Windows) in the npm prefix bin, while its configuration and upstream
        // state live in `~/.config/manicode/`. Either signal counts as installed.
        config_dirs: &[".config/manicode"],
    },
];

/// True if `binary` (plus any of `exts`) exists in one of the `path_dirs`.
/// `exts` always includes the empty string (the exact stem, e.g. a `claude`
/// shell script); on Windows it also carries the `PATHEXT` entries.
fn binary_on_path(
    binary: &str,
    path_dirs: &[PathBuf],
    exts: &[&str],
    exists: &dyn Fn(&Path) -> bool,
) -> bool {
    path_dirs.iter().any(|dir| {
        exts.iter()
            .any(|ext| exists(&dir.join(format!("{binary}{ext}"))))
    })
}

/// Pure detection over injected inputs — the unit-test seam (issue #536 AC5).
///
/// A harness is detected when any of its binary stems is found on `PATH`, or
/// when one of its home-relative config dirs exists. `exists` is the filesystem
/// probe (real `Path::exists` in production, a fake in tests).
pub fn detect_profiles(
    path_dirs: &[PathBuf],
    exts: &[&str],
    home: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<HarnessProfile> {
    DETECTABLE
        .iter()
        .filter(|d| {
            let on_path = d
                .binaries
                .iter()
                .any(|b| binary_on_path(b, path_dirs, exts, exists));
            let has_config = home.is_some_and(|h| d.config_dirs.iter().any(|c| exists(&h.join(c))));
            on_path || has_config
        })
        .map(|d| HarnessProfile {
            id: d.id.to_string(),
            name: d.name.to_string(),
            harness: d.harness.to_string(),
            runtime: None, wsl_distro: None,
        })
        .collect()
}

/// Binary-name extensions to try for a bare stem. Always includes `""` (the
/// exact name). On Windows we add each `PATHEXT` entry (e.g. `.EXE`, `.CMD`) so
/// `claude.exe` / `opencode.cmd` resolve — the same rule `where`/`which` apply.
/// The filesystem is case-insensitive there, so the original casing is fine.
fn path_exts() -> Vec<String> {
    let mut exts = vec![String::new()];
    if cfg!(windows) {
        match std::env::var("PATHEXT") {
            Ok(pe) if !pe.trim().is_empty() => exts.extend(
                pe.split(';')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            ),
            _ => exts.extend([".EXE", ".CMD", ".BAT", ".COM"].iter().map(|s| s.to_string())),
        }
    }
    exts
}

/// The user's home directory, mirroring [`crate::agent::provider::provider_conf`]'s
/// `USERPROFILE`-then-`HOME` resolution (avoids pulling in the `dirs` crate).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Standard global npm prefix bin locations to probe even if missing from PATH
/// (e.g. when PATH was stripped or user did not configure npm prefix in PATH).
fn standard_npm_bin_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
    }
    if let Some(home) = home {
        dirs.push(home.join(".npm-global").join("bin"));
        dirs.push(home.join(".npm").join("bin"));
        dirs.push(home.join("AppData").join("Roaming").join("npm"));
    }
    dirs
}

/// Real-filesystem entry point: scan `PATH`/`PATHEXT`, standard npm bin locations,
/// and the home config dirs for installed harnesses. Called once at startup from
/// `lib.rs` `setup()`.
pub fn detect_installed_profiles() -> Vec<HarnessProfile> {
    let mut path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = home_dir();
    for npm_dir in standard_npm_bin_dirs(home.as_deref()) {
        if !path_dirs.contains(&npm_dir) {
            path_dirs.push(npm_dir);
        }
    }
    let exts = path_exts();
    let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();
    // npm's Windows prefix also contains extensionless POSIX shims. They
    // belong to the Windows installation, not an independent Linux install.
    if crate::env::is_wsl_host() {
        path_dirs.retain(|dir| !DETECTABLE.iter().any(|tool| dir.join(format!("{}.cmd", tool.binaries[0])).is_file()));
        if let Ok(path) = std::env::join_paths(&path_dirs) { let _ = NATIVE_WSL_PATH.set(path); }
    }
    let mut profiles = detect_profiles(&path_dirs, &ext_refs, None, &|p| p.is_file());
    if cfg!(windows) {
        // Explicit Windows entries also work in WSL-backed meshes. Keep the
        // legacy entries so existing node identities retain their semantics.
        let native = profiles.iter().map(|p| runtime_profile(p, crate::models::EnvType::Windows));
        profiles.extend(native.collect::<Vec<_>>());
        profiles.extend(detect_wsl_profiles());
    }
    if crate::env::is_wsl_host() { profiles.extend(detect_windows_from_wsl()); }
    let _ = CURRENT_INSTALLATIONS.set(profiles.iter().map(|profile| profile.id.clone()).collect());
    profiles
}

static NATIVE_WSL_PATH: std::sync::OnceLock<std::ffi::OsString> = std::sync::OnceLock::new();

pub(crate) fn native_wsl_path() -> Option<&'static std::ffi::OsStr> {
    NATIVE_WSL_PATH.get().map(|path| path.as_os_str())
}

static CURRENT_INSTALLATIONS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

pub(crate) fn currently_installed_profiles(profiles: Vec<HarnessProfile>) -> Vec<HarnessProfile> {
    match CURRENT_INSTALLATIONS.get() {
        Some(ids) => filter_installed_profiles(profiles, ids),
        None => profiles,
    }
}

fn filter_installed_profiles(profiles: Vec<HarnessProfile>, ids: &[String]) -> Vec<HarnessProfile> {
    profiles.into_iter().filter(|p| !is_automatic_profile(p) || ids.contains(&p.id)).collect()
}

pub(crate) fn canonical_harness(id: &str) -> Option<&'static str> {
    DETECTABLE.iter().find(|tool| tool.id == id || tool.harness == id).map(|tool| tool.harness)
}

fn is_automatic_profile(p: &HarnessProfile) -> bool {
    DETECTABLE.iter().any(|tool| p.harness == tool.harness && (p.id == tool.id || p.id == format!("{}-windows", tool.id) || p.id.starts_with(&format!("{}-wsl-", tool.id))))
}

fn runtime_profile(profile: &HarnessProfile, runtime: crate::models::EnvType) -> HarnessProfile {
    let label = match runtime {
        crate::models::EnvType::Windows | crate::models::EnvType::WindowsInterop => "Windows",
        crate::models::EnvType::Wsl => "WSL",
    };
    HarnessProfile {
        id: format!("{}-{}", profile.id, label.to_ascii_lowercase()),
        name: format!("{} ({label})", profile.name),
        harness: profile.harness.clone(),
        runtime: Some(runtime), wsl_distro: None,
    }
}

/// Preserve custom profiles while choosing one installed runtime per built-in
/// harness. Canonical native ids stay stable for saved provider pairings.
pub(crate) fn preferred_profiles(
    profiles: &[HarnessProfile], host: crate::agent::provider::Platform, distro: Option<&str>,
) -> Vec<HarnessProfile> {
    use crate::agent::provider::Platform;
    use crate::models::EnvType;
    let automatic = |p: &HarnessProfile, tool: &Detectable| p.harness == tool.harness &&
        (p.id == tool.id || p.id == format!("{}-windows", tool.id) || p.id.starts_with(&format!("{}-wsl-", tool.id)));
    let mut result: Vec<_> = profiles.iter().filter(|p| !DETECTABLE.iter().any(|tool| automatic(p, tool))).cloned().collect();
    for tool in DETECTABLE {
        let candidates: Vec<_> = profiles.iter().filter(|p| automatic(p, tool)).collect();
        let chosen = candidates.iter().filter_map(|p| {
            let rank = match (host, p.runtime) {
                (Platform::Windows, Some(EnvType::Windows)) if crate::models::Provider::from_db_str(tool.harness).adapter().available_on().contains(&host) => 0,
                (_, None) if crate::models::Provider::from_db_str(tool.harness).adapter().available_on().contains(&host) => 1,
                (Platform::Windows, Some(EnvType::Wsl)) if p.wsl_distro.as_deref().is_none_or(|d| distro.is_none_or(|current| d == current)) => 2,
                (Platform::Linux, Some(EnvType::WindowsInterop)) => 2,
                _ => return None,
            };
            Some((rank, *p))
        }).min_by_key(|(rank, _)| *rank);
        if let Some((rank, chosen)) = chosen {
            let mut chosen = chosen.clone();
            if rank < 2 {
                if let Some(legacy) = candidates.iter().find(|p| p.id == tool.id) {
                    chosen.id = legacy.id.clone();
                    chosen.name = legacy.name.clone();
                } else { chosen.name = tool.name.into(); }
            }
            result.push(chosen);
        }
    }
    result
}

fn detect_windows_from_wsl() -> Vec<HarnessProfile> {
    let mut script = String::from("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); ");
    for tool in DETECTABLE {
        script.push_str(&format!("if (Get-Command '{}' -CommandType Application -ErrorAction SilentlyContinue) {{ [Console]::WriteLine('buildmesh-harness:{}') }}; ", if tool.id == "commandcode" { "cmdc" } else { tool.binaries[0] }, tool.id));
    }
    let command = crate::env::powershell_command(&script);
    let Ok(output) = crate::process_util::run_command_with_timeout(command, "Windows harness detection", std::time::Duration::from_secs(15)) else { return Vec::new(); };
    if !output.status.success() { return Vec::new(); }
    // Probe the actual mount roots once. Host-path readers never spawn tools.
    let mounts = ('a'..='z').filter_map(|drive| {
        let mut command = crate::process_util::command_no_window("wslpath");
        command.args(["-u", &format!("{drive}:\\")]);
        let output = crate::process_util::run_command_with_timeout(command, "WSL drive mount", std::time::Duration::from_secs(2)).ok()?;
        output.status.success().then(|| (drive, String::from_utf8_lossy(&output.stdout).trim().into()))
    }).collect();
    crate::env::set_wsl_drive_mounts(mounts);
    let stdout = String::from_utf8_lossy(&output.stdout);
    DETECTABLE.iter().filter(|tool| stdout.lines().any(|line| line == format!("buildmesh-harness:{}", tool.id)))
        .map(|tool| HarnessProfile { id: format!("{}-windows", tool.id), name: format!("{} (Windows)", tool.name),
            harness: tool.harness.into(), runtime: Some(crate::models::EnvType::WindowsInterop), wsl_distro: None })
        .collect()
}

/// One bounded login-shell probe, with executable checks rather than config
/// directory heuristics: a stale guest config must not advertise a launch.
fn detect_wsl_profiles() -> Vec<HarnessProfile> {
    let Some(distro) = crate::env::get_default_wsl_distro() else { return Vec::new(); };
    let mut script = String::from("export PATH=\"$HOME/.local/bin:$HOME/.npm-global/bin:$PATH\"; ");
    for drive in 'a'..='z' {
        script.push_str(&format!("mount=$(wslpath -u '{drive}:\\' 2>/dev/null) && printf 'buildmesh-mount:{drive}:%s\\n' \"$mount\"; "));
    }
    for tool in DETECTABLE {
        let binary = if tool.id == "commandcode" { "cmd" } else { tool.binaries[0] };
        script.push_str(&format!(
            "if command -v {binary} >/dev/null 2>&1; then printf 'buildmesh-harness:{}\\n'; fi; ",
            tool.id,
        ));
    }
    let mut command = crate::process_util::command_no_window("wsl.exe");
    command.args(["-d", &distro, "--cd", "~", "--exec", "sh", "-lc", &script]);
    match crate::process_util::run_command_with_timeout(command, "WSL harness detection", std::time::Duration::from_secs(10)) {
        Ok(output) if output.status.success() => {
            let output = String::from_utf8_lossy(&output.stdout);
            let mounts = output.lines().filter_map(|line| {
                let (drive, mount) = line.strip_prefix("buildmesh-mount:")?.split_once(':')?;
                let drive = drive.chars().next()?;
                (drive.is_ascii_lowercase() && mount.starts_with('/')).then(|| (drive, mount.to_string()))
            }).collect();
            crate::env::set_wsl_drive_mounts(mounts);
            profiles_from_wsl_probe(&output, &distro)
        },
        Ok(_) | Err(_) => Vec::new(),
    }
}

fn profiles_from_wsl_probe(output: &str, distro: &str) -> Vec<HarnessProfile> {
    DETECTABLE.iter().filter(|tool| output.lines().any(|line| line == format!("buildmesh-harness:{}", tool.id)))
        .map(|tool| {
            let mut profile = runtime_profile(&HarnessProfile {
                id: tool.id.into(), name: tool.name.into(), harness: tool.harness.into(), runtime: None, wsl_distro: None,
            }, crate::models::EnvType::Wsl);
            profile.id.push_str(&format!("-{}", hex::encode(distro.as_bytes())));
            profile.name = format!("{} (WSL: {distro})", tool.name);
            profile.wsl_distro = Some(distro.into());
            profile
        })
        .collect()
}

#[cfg(test)]
mod tests {

    #[test]
    fn removed_native_installation_exposes_foreign_fallback() {
        use crate::models::EnvType;
        use crate::agent::provider::Platform;
        let profiles = vec![
            crate::preferences::HarnessProfile { id: "mcode".into(), name: "MiniMax Code".into(), harness: "mcode".into(), runtime: None, wsl_distro: None },
            crate::preferences::HarnessProfile { id: "mcode-windows".into(), name: "MiniMax Code (Windows)".into(), harness: "mcode".into(), runtime: Some(EnvType::Windows), wsl_distro: None },
            crate::preferences::HarnessProfile { id: "mcode-wsl-test".into(), name: "MiniMax Code (WSL)".into(), harness: "mcode".into(), runtime: Some(EnvType::Wsl), wsl_distro: None },
            crate::preferences::HarnessProfile { id: "custom".into(), name: "Custom".into(), harness: "anthropic".into(), runtime: None, wsl_distro: None },
        ];
        let installed = super::filter_installed_profiles(profiles, &["mcode-wsl-test".into()]);
        let menu = super::preferred_profiles(&installed, Platform::Windows, None);
        assert_eq!(menu.len(), 2);
        assert_eq!(menu.iter().find(|p| p.harness == "mcode").unwrap().runtime, Some(EnvType::Wsl));
        assert!(menu.iter().any(|p| p.id == "custom"));
    }

    #[test]
    fn preferred_installations_deduplicate_native_and_cross_runtime_copies() {
        use crate::models::EnvType;
        use crate::agent::provider::Platform;
        let profile = |id: &str, harness: &str, runtime| crate::preferences::HarnessProfile {
            id: id.into(), name: if harness == "mcode" { "MiniMax Code".into() } else { "Meta Muse (WSL)".into() },
            harness: harness.into(), runtime, wsl_distro: None,
        };
        let profiles = vec![profile("mcode", "mcode", None), profile("mcode-windows", "mcode", Some(EnvType::Windows)),
            profile("mcode-wsl-test", "mcode", Some(EnvType::Wsl)), profile("muse-wsl-test", "muse", Some(EnvType::Wsl))];
        let menu = super::preferred_profiles(&profiles, Platform::Windows, Some("Ubuntu"));
        assert_eq!(menu.len(), 2);
        let mcode = menu.iter().find(|p| p.harness == "mcode").unwrap();
        assert_eq!(mcode.id, "mcode");
        assert_eq!(mcode.name, "MiniMax Code");
        assert_eq!(mcode.runtime, Some(EnvType::Windows));
        assert_eq!(menu.iter().find(|p| p.harness == "muse").unwrap().runtime, Some(EnvType::Wsl));
        let mut linux = profiles;
        linux.iter_mut().find(|p| p.id == "mcode-windows").unwrap().runtime = Some(EnvType::WindowsInterop);
        linux.push(profile("grok-windows", "grok", Some(EnvType::WindowsInterop)));
        let menu = super::preferred_profiles(&linux, Platform::Linux, None);
        assert_eq!(menu.len(), 2);
        assert_eq!(menu.iter().find(|p| p.harness == "mcode").unwrap().runtime, None);
        assert_eq!(menu.iter().find(|p| p.harness == "grok").unwrap().runtime, Some(EnvType::WindowsInterop));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires Muse installed in the default WSL distribution"]
    fn live_wsl_detection_finds_muse_and_drive_mounts() {
        let profiles = super::detect_wsl_profiles();
        let muse = profiles.iter().find(|p| p.harness == "muse").expect("Muse must be discovered");
        assert_eq!(muse.runtime, Some(crate::models::EnvType::Wsl));
        assert_eq!(muse.wsl_distro, crate::env::get_default_wsl_distro());
        let host = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let guest = crate::env::windows_to_wsl(&host);
        let mut command = crate::process_util::command_no_window("wsl.exe");
        command.args(["-d", muse.wsl_distro.as_deref().unwrap(), "--exec", "wslpath", "-u", &host]);
        let output = crate::process_util::run_command_with_timeout(command, "verify mount", std::time::Duration::from_secs(10)).unwrap();
        assert!(output.status.success());
        assert_eq!(guest, String::from_utf8(output.stdout).unwrap().trim());
        assert_eq!(std::fs::canonicalize(crate::env::to_host_path(&guest)).unwrap(), std::fs::canonicalize(host).unwrap());
    }
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn wsl_probe_only_advertises_known_executables_with_explicit_runtime() {
        let profiles = profiles_from_wsl_probe("welcome\nbuildmesh-harness:muse\nbuildmesh-harness:codex\nbuildmesh-harness:unknown\nbuildmesh-harness:muse\n", "Ubuntu");
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["muse-wsl-5562756e7475", "codex-wsl-5562756e7475"]);
        assert_eq!(profiles[0].name, "Meta Muse (WSL: Ubuntu)");
        assert_eq!(profiles[0].wsl_distro.as_deref(), Some("Ubuntu"));
        assert_eq!(profiles[0].harness, "muse");
        assert_eq!(profiles[0].runtime, Some(crate::models::EnvType::Wsl));
        assert!(profiles_from_wsl_probe("command not found", "Ubuntu").is_empty());
    }

    /// Build an `exists` closure that reports the given paths (as strings) as
    /// present and everything else as absent. Backslashes are normalised to `/`
    /// so fixtures can use plain `/`-joined strings, and matching is
    /// case-insensitive to model the Windows filesystem `PATHEXT` relies on
    /// (a binary `claude.exe` on disk satisfies the `.EXE` extension probe).
    fn fake_fs(present: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<String> = present
            .iter()
            .map(|s| s.replace('\\', "/").to_lowercase())
            .collect();
        move |p: &Path| set.contains(&p.to_string_lossy().replace('\\', "/").to_lowercase())
    }

    fn dirs(parts: &[&str]) -> Vec<PathBuf> {
        parts.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn detects_binary_present_on_path() {
        let path_dirs = dirs(&["/usr/local/bin", "/usr/bin"]);
        let exists = fake_fs(&["/usr/local/bin/claude"]);
        let profiles = detect_profiles(&path_dirs, &[""], None, &exists);
        let ids: Vec<_> = profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["claude"]);
        assert_eq!(profiles[0].name, "Claude Code");
        assert_eq!(profiles[0].harness, "anthropic");
    }

    #[test]
    fn flags_absent_binary_as_not_detected() {
        let path_dirs = dirs(&["/usr/bin"]);
        // Nothing on disk → no profiles at all.
        let exists = fake_fs(&[]);
        let profiles = detect_profiles(&path_dirs, &[""], None, &exists);
        assert!(profiles.is_empty(), "no binaries present → no profiles");
    }

    #[test]
    fn detects_only_the_present_subset() {
        let path_dirs = dirs(&["/bin"]);
        let exists = fake_fs(&["/bin/codex", "/bin/opencode"]);
        let mut ids: Vec<_> = detect_profiles(&path_dirs, &[""], None, &exists)
            .into_iter()
            .map(|p| p.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["codex", "opencode"]);
    }

    #[test]
    fn detects_cursor_agent_binary_without_matching_cursor_ide_binary() {
        let path_dirs = dirs(&["/bin"]);
        let exists = fake_fs(&["/bin/cursor-agent", "/bin/cursor"]);
        let profiles = detect_profiles(&path_dirs, &[""], None, &exists);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "cursor");
        assert_eq!(profiles[0].name, "Cursor Agent");
        assert_eq!(profiles[0].harness, "cursor");
    }

    #[test]
    fn cursor_config_dir_alone_counts_as_installed() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.cursor"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(
            profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["cursor"]
        );
    }

    #[test]
    fn windows_extension_resolves_the_binary() {
        // `claude` itself isn't on disk, but `claude.exe` (via PATHEXT) is.
        let path_dirs = dirs(&["C:/tools"]);
        let exists = fake_fs(&["C:/tools/claude.exe"]);
        let profiles = detect_profiles(&path_dirs, &["", ".EXE"], None, &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["claude"]);
    }

    #[test]
    fn config_dir_alone_counts_as_installed() {
        // No binary on PATH, but ~/.claude exists → Claude Code still offered.
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.claude"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["claude"]);
    }

    /// Kimi Code (#918) ships `~/.kimi/` for config alongside the `kimi` binary.
    /// A shell-function/alias install that exposes only the config dir (no
    /// PATH entry) must still surface as a Kimi Code harness — same
    /// rationale as the Claude config-dir test above.
    #[test]
    fn kimi_config_dir_alone_counts_as_installed() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.kimi"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["kimi"]);
    }

    /// MiniMax Code CLI (`mcode`) ships `~/.mcode/` or `~/.minimax-code/` for config alongside the `mcode` binary.
    #[test]
    fn mcode_config_dir_alone_counts_as_installed() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.mcode"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["mcode"]);

        let exists_alt = fake_fs(&["/home/me/.minimax-code"]);
        let profiles_alt = detect_profiles(&path_dirs, &[""], Some(&home), &exists_alt);
        assert_eq!(profiles_alt.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["mcode"]);
    }

    /// DeepSeek Harness (`dsh`) ships `~/.dsh/` or `~/.deepseek-harness/` for config alongside the `dsh` binary.
    #[test]
    fn dsh_config_dir_alone_counts_as_installed() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.dsh"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["dsh"]);

        let exists_alt = fake_fs(&["/home/me/.deepseek-harness"]);
        let profiles_alt = detect_profiles(&path_dirs, &[""], Some(&home), &exists_alt);
        assert_eq!(profiles_alt.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["dsh"]);
    }

    /// Command Code ships `~/.commandcode/` for config and auth credentials alongside the `cmdc`/`cmd` binary.
    #[test]
    fn commandcode_config_dir_alone_counts_as_installed() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.commandcode"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["commandcode"]);
    }

    /// On Windows, `cmd.exe` in System32 must NOT false-positive Command Code.
    #[test]
    #[cfg(windows)]
    fn windows_cmd_exe_alone_does_not_detect_commandcode() {
        let path_dirs = dirs(&["C:/Windows/System32"]);
        let exists = fake_fs(&["C:/Windows/System32/cmd.exe"]);
        let profiles = detect_profiles(&path_dirs, &["", ".EXE", ".CMD"], None, &exists);
        assert!(
            !profiles.iter().any(|p| p.id == "commandcode"),
            "System32/cmd.exe must NOT detect Command Code on Windows; got {profiles:?}"
        );
    }

    /// On Windows, `cmdc.cmd` on PATH must detect Command Code.
    #[test]
    #[cfg(windows)]
    fn windows_cmdc_detects_commandcode() {
        let path_dirs = dirs(&["C:/Users/me/AppData/Roaming/npm"]);
        let exists = fake_fs(&["C:/Users/me/AppData/Roaming/npm/cmdc.cmd"]);
        let profiles = detect_profiles(&path_dirs, &["", ".CMD", ".EXE"], None, &exists);
        assert!(
            profiles.iter().any(|p| p.id == "commandcode"),
            "cmdc.cmd must detect Command Code on Windows; got {profiles:?}"
        );
    }

    /// On Unix, `cmd` on PATH must detect Command Code, while `cmdc` alone does not.
    #[test]
    #[cfg(not(windows))]
    fn unix_cmd_detects_commandcode() {
        let path_dirs = dirs(&["/usr/local/bin"]);
        let exists = fake_fs(&["/usr/local/bin/cmd"]);
        let profiles = detect_profiles(&path_dirs, &[""], None, &exists);
        assert!(
            profiles.iter().any(|p| p.id == "commandcode"),
            "cmd on Unix must detect Command Code; got {profiles:?}"
        );

        let exists_cmdc = fake_fs(&["/usr/local/bin/cmdc"]);
        let profiles_cmdc = detect_profiles(&path_dirs, &[""], None, &exists_cmdc);
        assert!(
            !profiles_cmdc.iter().any(|p| p.id == "commandcode"),
            "cmdc alone on Unix must NOT detect Command Code; got {profiles_cmdc:?}"
        );
    }

    /// Antigravity (issue #1287) declares Antigravity-only config dirs
    /// (`.gemini/antigravity-cli/`, `.antigravity/`, `.antigravitycli/`).
    /// A bare `.agy` directory is NOT one of them, so a stray `.agy`
    /// subfolder in $HOME can't conjure it — the same anti-false-positive
    /// discipline the cursor-agent stem note at the top of this file
    /// applies. Regression pin for the explicit `.agy` exclusion: if a
    /// future change ever adds `.agy` as a declared config dir, this test
    /// trips and the reviewer is forced to consider the false-positive
    /// surface that opens up.
    #[test]
    fn agy_does_not_recognise_a_bare_dot_agy_dir() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.agy"]); // not a declared config dir
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert!(
            profiles.is_empty(),
            ".agy/ alone must NOT detect Antigravity (false-positive risk — \
             e.g. another tool using that name); got {profiles:?}"
        );
    }

    /// Antigravity (issue #1287) installs its CLI under
    /// `~/.gemini/antigravity-cli/`. A shell-function / alias install that
    /// exposes only the config dir (no PATH entry) must still surface as
    /// an Antigravity harness. Mirrors the rationale in the kimi and
    /// mcode config-dir tests above.
    #[test]
    fn agy_config_dir_alone_counts_as_installed() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.gemini/antigravity-cli"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["agy"]);

        // The IDE-fork home and the symlink-layer home must count too —
        // mirrors the dual-`.kimi` / dual-`.mcode` pattern used for
        // installs that surface under more than one name.
        let exists_alt = fake_fs(&["/home/me/.antigravity"]);
        let profiles_alt = detect_profiles(&path_dirs, &[""], Some(&home), &exists_alt);
        assert_eq!(
            profiles_alt.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["agy"]
        );

        let exists_sym = fake_fs(&["/home/me/.antigravitycli"]);
        let profiles_sym = detect_profiles(&path_dirs, &[""], Some(&home), &exists_sym);
        assert_eq!(
            profiles_sym.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["agy"]
        );
    }

    /// Antigravity's detection signals are scoped to Antigravity-specific
    /// subdirs — `.gemini/` *alone* is deliberately NOT in the list, because
    /// that directory is shared with Google Gemini CLI. A user with only
    /// Gemini CLI installed (no Antigravity CLI anywhere) must not see
    /// Antigravity appear in the launch menu — false positives there cascade
    /// into a launch-time failure ("command not found") that no schema
    /// check would catch.
    #[test]
    fn agy_does_not_recognise_a_bare_dot_gemini_dir() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        // ~/.gemini exists but ~/.gemini/antigravity-cli does not — Gemini
        // CLI only, no Antigravity. Must NOT detect.
        let exists = fake_fs(&["/home/me/.gemini"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert!(
            profiles.is_empty(),
            ".gemini/ alone must NOT detect Antigravity (shared with Gemini CLI); \
             got {profiles:?}"
        );
    }

    /// Freebuff (issue #1437) stores its upstream config under `~/.config/manicode/`.
    /// A shell-function or alias install that exposes only the config dir (no
    /// PATH entry) must still surface as a Freebuff harness.
    #[test]
    fn freebuff_config_dir_alone_counts_as_installed() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.config/manicode"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["freebuff"]);
    }

    /// Freebuff installed globally via npm in `%APPDATA%\npm` or `~/.npm-global/bin`
    /// must be detected when that directory is on the search path.
    #[test]
    fn freebuff_npm_prefix_bin_detected() {
        let path_dirs = dirs(&["C:/Users/me/AppData/Roaming/npm"]);
        let exists = fake_fs(&["C:/Users/me/AppData/Roaming/npm/freebuff.cmd"]);
        let profiles = detect_profiles(&path_dirs, &["", ".CMD", ".EXE"], None, &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["freebuff"]);
    }

    #[test]
    fn detected_ids_resolve_to_their_legacy_provider() {
        use crate::models::Provider;
        // The harness field of every detectable maps to a real legacy provider.
        let path_dirs = dirs(&["/bin"]);
        let exists = fake_fs(&[
            "/bin/claude",
            "/bin/codex",
            "/bin/cursor-agent",
            "/bin/agy",
            "/bin/opencode",
            "/bin/grok",
            "/bin/kimi",
            "/bin/mcode",
            "/bin/dsh",
            "/bin/cmdc",
            "/bin/cmd",
            "/bin/freebuff",
        ]);
        let profiles = detect_profiles(&path_dirs, &[""], None, &exists);
        for p in &profiles {
            // from_db_str never errs; assert the harness isn't an accidental typo
            // by checking it round-trips to a non-default variant where expected.
            let provider = Provider::from_db_str(&p.harness);
            match p.id.as_str() {
                "claude" => assert_eq!(provider, Provider::Anthropic),
                "codex" => assert_eq!(provider, Provider::Codex),
                "cursor" => assert_eq!(provider, Provider::Cursor),
                "agy" => assert_eq!(provider, Provider::Agy),
                "opencode" => assert_eq!(provider, Provider::OpenCode),
                "grok" => assert_eq!(provider, Provider::Grok),
                "kimi" => assert_eq!(provider, Provider::Kimi),
                "mcode" => assert_eq!(provider, Provider::Mcode),
                "dsh" => assert_eq!(provider, Provider::Dsh),
                "commandcode" => assert_eq!(provider, Provider::CommandCode),
                "freebuff" => assert_eq!(provider, Provider::Freebuff),
                other => panic!("unexpected detected id {other}"),
            }
        }
    }

    #[test]
    fn detect_profiles_against_a_real_temp_filesystem() {
        // End-to-end over the real `Path::exists` probe (not the fake closure):
        // a present binary is detected, an absent one is not. Hermetic — uses a
        // unique temp dir keyed on the process id and cleans up after itself.
        let root = std::env::temp_dir().join(format!("bm-detect-test-{}", std::process::id()));
        let bin_dir = root.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        // Only `codex` exists on disk; `claude`/`agy`/`opencode` do not.
        std::fs::write(bin_dir.join("codex"), b"#!/bin/sh\n").unwrap();

        let profiles = detect_profiles(
            std::slice::from_ref(&bin_dir),
            &[""],
            None,
            &|p: &std::path::Path| p.exists(),
        );

        let ids: Vec<_> = profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["codex"], "only the on-disk binary is detected");

        let _ = std::fs::remove_dir_all(&root);
    }
}
