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
        id: "agy",
        name: "Antigravity",
        harness: "agy",
        binaries: &["agy"],
        config_dirs: &[],
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
        config_dirs: &[".mcode"],
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

/// Real-filesystem entry point: scan `PATH`/`PATHEXT` and the home config dirs
/// for installed harnesses. Called once at startup from `lib.rs` `setup()`.
pub fn detect_installed_profiles() -> Vec<HarnessProfile> {
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let exts = path_exts();
    let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();
    let home = home_dir();
    detect_profiles(&path_dirs, &ext_refs, home.as_deref(), &|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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

    /// MiniMax Code CLI (`mcode`) ships `~/.mcode/` for config alongside the `mcode` binary.
    #[test]
    fn mcode_config_dir_alone_counts_as_installed() {
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.mcode"]);
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert_eq!(profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["mcode"]);
    }

    #[test]
    fn agy_has_no_config_dir_so_needs_the_binary() {
        // Antigravity declares no config dir, so a stray home dir can't conjure
        // it — only a binary on PATH detects it.
        let path_dirs = dirs(&["/usr/bin"]);
        let home = PathBuf::from("/home/me");
        let exists = fake_fs(&["/home/me/.agy"]); // not a declared config dir
        let profiles = detect_profiles(&path_dirs, &[""], Some(&home), &exists);
        assert!(profiles.is_empty());
    }

    #[test]
    fn detected_ids_resolve_to_their_legacy_provider() {
        use crate::models::Provider;
        // The harness field of every detectable maps to a real legacy provider.
        let path_dirs = dirs(&["/bin"]);
        let exists = fake_fs(&[
            "/bin/claude",
            "/bin/codex",
            "/bin/agy",
            "/bin/opencode",
            "/bin/grok",
            "/bin/kimi",
            "/bin/mcode",
        ]);
        let profiles = detect_profiles(&path_dirs, &[""], None, &exists);
        for p in &profiles {
            // from_db_str never errs; assert the harness isn't an accidental typo
            // by checking it round-trips to a non-default variant where expected.
            let provider = Provider::from_db_str(&p.harness);
            match p.id.as_str() {
                "claude" => assert_eq!(provider, Provider::Anthropic),
                "codex" => assert_eq!(provider, Provider::Codex),
                "agy" => assert_eq!(provider, Provider::Agy),
                "opencode" => assert_eq!(provider, Provider::OpenCode),
                "grok" => assert_eq!(provider, Provider::Grok),
                "kimi" => assert_eq!(provider, Provider::Kimi),
                "mcode" => assert_eq!(provider, Provider::Mcode),
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
