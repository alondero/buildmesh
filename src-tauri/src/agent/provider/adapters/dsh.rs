//! DeepSeek Harness CLI provider adapter (`dsh`).
//!
//! `dsh` is a launcher, not a single TUI binary: per the upstream
//! `apps/cli/README.md`, it parses only its own flags (`--profile`,
//! `--patch`, `--dump-default-config`, `--dump-config`, `--help`)
//! and forwards everything else to the booted profile
//! (`web`/`headless`/`sdk`/`sdk-minimal`/`acp`/custom).
//!
//! Issue #1365: no profile is validated, so we gate the capability
//! flags that depend on profile acceptance (resume, model override,
//! attention, transcript, prefill). The orchestrator's
//! `SessionIdMode` plumbing (prepare.rs:249) routes `None` when
//! `supports_resume = false`, so `session_assign_args` and
//! `resume_args` are never called. The default wire formatters
//! apply if a maintainer flips a capability back to true.
//!
//! **Shell wrapping**: `dsh.cmd` (Windows) is a `.cmd` shim that
//! `CreateProcess` won't run directly — `WindowsShell::Cmd` →
//! `cmd.exe /c dsh …`. macOS / Linux use `WindowsShell::Direct`.

use crate::agent::provider::{shell_for, AgentProvider, Platform, SpawnRecipe, UiMeta};
// The recipe tests assert the shell each platform produces; nothing outside
// `cfg(test)` names the type now that `shell_for` is shared.
#[cfg(test)]
use crate::agent::provider::WindowsShell;
use crate::models::EnvType;

pub struct DshAdapter;
pub static DSH: DshAdapter = DshAdapter;

impl AgentProvider for DshAdapter {
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: false,
                auto_resume_on_startup: false,
                produces_readable_transcript: false,
                supports_model_override: false,
                supports_extra_args: {
                    // Escape hatch for users to manually inject `--profile <name>`
                    // (or anything else the launcher accepts) until the
                    // capability layer knows which profile to advertise. The
                    // user's string is tokenised by `extra_args_args` and
                    // appended verbatim — whether bare `dsh` accepts the
                    // resulting argv is unverified.
                    true
                },
                supports_prefill: false,
                available_on: { &[Platform::Windows, Platform::Linux, Platform::Macos] },
                ..Default::default()
            });
        &DESCRIPTOR
    }

    fn id(&self) -> &'static str {
        "dsh"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "DeepSeek Harness".into(),
            color: "#1E88E5".into(),
            icon: "D".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "dsh",
            base_args: Vec::new(),
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    // Gated to false pending profile validation (issue #1365).
    // When a maintainer flips a profile in, every false here needs
    // re-evaluation and the inventory pins in
    // `agent::capabilities::tests::inventory_matches_research_matrix`
    // + `models::tests::provider_capabilities_split_correctly` need
    // to flip too.

    fn self_assigns_session_id(&self) -> bool {
        false
    }
}

#[cfg(test)]
#[path = "dsh_tests.rs"]
mod tests;
