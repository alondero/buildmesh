//! Provider-binary preflight (issue #823).
//!
//! A missing agent CLI used to surface as a red toast or, worse, as
//! `'claude' is not recognized` scrolled inside a freshly-opened terminal —
//! the user had to read PTY output to learn that nothing was wrong with the
//! mesh, the branch or the credentials. This module resolves the binary the
//! spawn is about to invoke *before* the PTY opens, so a miss becomes a
//! structured `provider-error` toast instead of a cryptic terminal.
//!
//! ## What is checked
//!
//! Exactly the executable [`crate::agent::spawn_environment::wrap`] will
//! dispatch, in the same order it prefers them:
//!
//! 1. the routing's [`PreparedLaunchRouting::executable_override`] (an
//!    absolute path detection already resolved), then
//! 2. the adapter's recipe stem re-resolved through
//!    [`crate::agent::detection::resolve_spawn_binary`] — the enriched search
//!    path (`PATH` + npm prefix bins + user bin dirs + NVM shims), which is a
//!    *superset* of what a GUI-launched process `PATH` lookup would find.
//!
//! Because the enriched search is a superset of the process `PATH`, a miss
//! here means the shell could not have found it either: the preflight never
//! rejects a spawn that would have worked.
//!
//! ## What is deliberately NOT checked
//!
//! * **`Provider::Terminal`** — its "binary" is the user's shell
//!   (`powershell.exe` / `sh`), which always exists and is never something
//!   the user installs. A miss message would be unactionable.
//! * **Guest-side runtimes** (`EnvType::Wsl`, `EnvType::WindowsInterop`) — the
//!   binary is resolved *inside* the guest login shell (or on the Windows side
//!   of an interop spawn). Host-side filesystem probing cannot answer that
//!   question, and a `/home/...` guest path probed against the Windows host
//!   would report a false miss. Preflight skips rather than guesses, exactly
//!   as [`crate::agent::launch_routing::spawn_time_executable`] drops its host
//!   override for those runtimes.
//!
//! The routing may pin a runtime of its own (Codex proxy selects its install
//! during prepare), so the check runs against
//! [`PreparedLaunchRouting::pinned_runtime`] when present and the mesh path's
//! runtime otherwise.

use std::path::Path;

use crate::agent::launch_routing::PreparedLaunchRouting;
use crate::models::EnvType;
use crate::models::Provider;

/// Outcome of the preflight.
pub(super) enum Preflight {
    /// The binary the spawn will invoke resolves. Launch proceeds.
    Ready,
    /// No executable for this spawn, with the user-facing explanation. The
    /// caller emits it as a `provider-error` and skips the PTY launch.
    Missing(String),
}

/// Resolve the executable this spawn would invoke, or explain why it cannot.
///
/// Runs before the PTY opens and performs no database access, so a miss
/// fails fast on the spawn hot path. See the module docs for the resolution
/// order and the exempted cases.
pub(super) fn ensure_spawn_binary(
    provider: Provider,
    host_env_type: EnvType,
    routing: &PreparedLaunchRouting,
) -> Preflight {
    // A plain shell is not something a user installs. Checking it would only
    // ever produce an unactionable message.
    if provider == Provider::Terminal {
        return Preflight::Ready;
    }

    // Guest-side spawns resolve the stem in another runtime's shell, which
    // host-side probing cannot observe.
    let env_type = routing.pinned_runtime().unwrap_or(host_env_type);
    if matches!(env_type, EnvType::Wsl | EnvType::WindowsInterop) {
        return Preflight::Ready;
    }

    check_spawn_binary(
        provider,
        routing.executable_override(),
        &|path| path.is_file(),
        &|stem| crate::agent::detection::resolve_spawn_binary(stem),
    )
}

/// The decision, with the filesystem probe injected so the unit tests can
/// exercise both branches without touching a real disk or the real `PATH`.
pub(super) fn check_spawn_binary(
    provider: Provider,
    executable_override: Option<&Path>,
    is_file: &dyn Fn(&Path) -> bool,
    resolve_stem: &dyn Fn(&str) -> Option<std::path::PathBuf>,
) -> Preflight {
    if let Some(path) = executable_override {
        // The routing already resolved an absolute path, so `wrap` will
        // dispatch that exact file rather than re-resolving the stem. If the
        // file is gone (uninstalled or replaced since detection), that
        // dispatch would fail at exec time.
        return if is_file(path) {
            Preflight::Ready
        } else {
            Preflight::Missing(missing_override_message(provider, path))
        };
    }

    // No override: `wrap` falls back to the bare recipe stem and lets the
    // shell search for it. Resolve the same stem through the enriched search
    // to learn whether that lookup would succeed.
    let stem = crate::agent::launch_routing::recipe_binary_for(provider, EnvType::Windows);
    if resolve_stem(stem).is_some() {
        Preflight::Ready
    } else {
        Preflight::Missing(missing_stem_message(provider, stem))
    }
}

fn missing_override_message(provider: Provider, path: &Path) -> String {
    format!(
        "{} can't start: its executable is no longer on disk ({}). Reinstall the CLI, then start the node again.",
        harness_label(provider),
        path.display(),
    )
}

fn missing_stem_message(provider: Provider, stem: &str) -> String {
    format!(
        "{} can't start: the `{}` command wasn't found. Install the CLI and make sure it's on your PATH, then start the node again.",
        harness_label(provider),
        stem,
    )
}

/// User-facing name for the harness, from the adapter's own UI metadata so
/// the message matches the row the user clicked.
fn harness_label(provider: Provider) -> String {
    provider.adapter().ui().label
}
