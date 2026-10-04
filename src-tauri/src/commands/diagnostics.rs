//! Read-only access to the diagnostic locations this process resolved at
//! startup (issue #1525).
//!
//! Why a command
//! -------------
//! The fatal-startup path already shows the user an absolute log path, but
//! that surface is native and only exists when startup *failed*. The Boot
//! Error Panel covers the other half of the problem: a backend that booted
//! fine and then rejected one of `App.init()`'s IPC calls (#1250). That panel
//! used to promise details were in "buildmesh.log" — a bare filename in a
//! sentence with no path — so a user reporting a boot error could not find the
//! file without already knowing where Buildmesh keeps its data.
//!
//! This command closes that gap by handing the panel the resolved absolute
//! locations. The values come from the [`crate::startup`] bootstrap, which
//! already resolved and opened them before the database, so this performs no
//! I/O of its own and cannot disagree with the files that were actually
//! written.

use serde::Serialize;
use tauri::command;
use ts_rs::TS;

/// The absolute diagnostic locations for the running process.
///
/// Generated to `src/types/generated/DiagnosticPaths.ts`; the frontend imports
/// it rather than restating the shape (see ADR-0009). Strings, not paths,
/// because these cross the IPC boundary as text to be displayed verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "DiagnosticPaths.ts")]
pub struct DiagnosticPaths {
    /// The app-data profile directory (database, preferences, lock files).
    pub profile_dir: String,
    /// The `logs` directory.
    pub log_dir: String,
    /// The size-bounded main log, `buildmesh.log`.
    pub main_log: String,
    /// The panic log. It is written by a hook that does not go through
    /// `tracing`, so it is a separate file and is listed separately rather
    /// than being presented as part of the main log.
    pub panic_log: String,
}

/// The resolved diagnostic locations for this process.
///
/// Errors only when the bootstrap never installed — which cannot happen for a
/// running app, since every command call implies `setup` completed. It is
/// reported rather than defaulted because a fabricated path would send a user
/// to a file that does not exist, which is the exact failure this command
/// exists to remove.
#[command]
pub fn get_diagnostic_paths() -> Result<DiagnosticPaths, String> {
    let paths = crate::startup::installed_paths().ok_or_else(|| {
        "Buildmesh's diagnostic paths are not available in this process".to_string()
    })?;
    Ok(DiagnosticPaths::from_resolved(&paths))
}

impl DiagnosticPaths {
    /// Derive the wire shape from the bootstrap's resolved paths.
    ///
    /// Split out from the command so the derivation is testable without a
    /// booted app: `panic.log` in particular is *derived* from the `logs`
    /// directory rather than resolved separately, because a hard-coded relative
    /// name is exactly the bug that left the old Boot Error Panel unable to
    /// tell a user where the file was.
    fn from_resolved(paths: &crate::startup::ProfilePaths) -> Self {
        Self {
            profile_dir: paths.profile_dir.display().to_string(),
            log_dir: paths.log_dir.display().to_string(),
            main_log: paths.main_log.display().to_string(),
            panic_log: paths.log_dir.join("panic.log").display().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile layout under the platform's real temp directory.
    ///
    /// Built from `temp_dir()` rather than a literal `/profiles/stable`,
    /// because `Path::is_absolute` is platform-specific: a leading `/` is
    /// absolute on Unix and *not* absolute on Windows, so a hard-coded POSIX
    /// path would have made this test pass on Linux and fail here.
    fn sample_paths() -> crate::startup::ProfilePaths {
        let profile_dir = std::env::temp_dir().join("bm-profile");
        let log_dir = profile_dir.join("logs");
        crate::startup::ProfilePaths {
            main_log: log_dir.join("buildmesh.log"),
            log_dir,
            profile_dir,
        }
    }

    /// Every location must be absolute and inside the one profile, so a user
    /// can paste any of them into a file manager and get there. A relative or
    /// profile-less path is the defect this command exists to remove.
    #[test]
    fn every_location_is_absolute_and_inside_the_profile() {
        let paths = sample_paths();
        let wire = DiagnosticPaths::from_resolved(&paths);
        let profile = paths.profile_dir.display().to_string();

        for (field, value) in [
            ("profile_dir", &wire.profile_dir),
            ("log_dir", &wire.log_dir),
            ("main_log", &wire.main_log),
            ("panic_log", &wire.panic_log),
        ] {
            assert!(!value.is_empty(), "{field} must not be empty");
            assert!(
                std::path::Path::new(value).is_absolute(),
                "{field} must be absolute, got {value}"
            );
            assert!(
                value.starts_with(&profile),
                "{field} must live under the resolved profile, got {value}"
            );
        }
    }

    /// The panel promises a specific file; the name is a contract with the
    /// skills and `scripts/*log*.ps1` that tail it, so it is pinned here
    /// rather than left to whatever the bootstrap happens to join.
    #[test]
    fn the_two_log_names_match_what_the_backend_opens() {
        let wire = DiagnosticPaths::from_resolved(&sample_paths());
        assert!(
            wire.main_log.ends_with("buildmesh.log"),
            "got {}",
            wire.main_log
        );
        assert!(
            wire.panic_log.ends_with("panic.log"),
            "got {}",
            wire.panic_log
        );
        // The two logs are siblings, not nested: the panic hook writes its own
        // file and never goes through the tracing pipeline.
        assert_eq!(
            std::path::Path::new(&wire.panic_log).parent(),
            std::path::Path::new(&wire.main_log).parent(),
            "panic.log must sit beside buildmesh.log"
        );
    }

    /// In a test process the bootstrap has not run, so the command must say
    /// so rather than hand back a plausible-looking but fictional path.
    #[test]
    fn an_unbootstrapped_process_reports_no_paths() {
        if crate::startup::installed_paths().is_some() {
            return;
        }
        let error = get_diagnostic_paths()
            .expect_err("a process with no bootstrap has no paths to report");
        assert!(
            error.contains("not available"),
            "the frontend needs to be able to recognise this, got {error}"
        );
    }
}
