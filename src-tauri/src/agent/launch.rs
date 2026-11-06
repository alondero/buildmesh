//! Adapter-dispatched Agent Harness launch construction.
//!
//! Production and behavioral tests call [`AgentProvider::prepare_launch`].
//! [`PreparedHarnessLaunch`] carries the recipe, advertised capability contract
//! and environment contribution from that selected adapter. [`default_prepare`]
//! is shared implementation reuse for base-recipe plus option assembly; adapters
//! own resolved routing spelling and can override the dispatched construction.
//! Capability decisions come from the authoritative descriptor.
//!
//! The resume *shape* is adapter-owned too: [`SessionIdModeRef::Resume`]
//! carries the cause, and `Regenerate` composes
//! [`AgentProvider::resume_args_fork`] instead of
//! [`AgentProvider::resume_args`] so a same-harness regenerate branches off
//! the captured transcript rather than extending it (issue #1550). The
//! default fork shape equals the default resume shape, so only adapters
//! that actually advertise a fork opt in.
//!
//! The capability contract travels with the recipe so a single test
//! (`spawn::command_tests::capability_recipe_coherence`) can prove they agree:
//! for every adapter, every session mode, and every value the resolver
//! might forward, the final `recipe.base_args` either contains the
//! expected flag or — when the capability descriptor says the harness
//! does not support the control — never does.

use crate::agent::capabilities::{EffortControlKind, ResolvedAgentConfig};
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe};
use crate::agent::spawn::ResumeCause;
use crate::models::EnvType;

/// A `SessionIdMode` reference. Owns no allocation; borrows the orchestrator's
/// mode value. Split out from `agent::spawn::SessionIdMode` (which owns the
/// `String`) because the launch helper is pure and must not allocate — the
/// orchestrator has already done so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIdModeRef<'a> {
    /// Assign a new session id (orchestrator pre-writes it; harness CLI
    /// consumes it via `--session-id <uuid>` or the adapter's equivalent).
    Assign(&'a str),
    /// Resume an existing session id (orchestrator already has it; harness
    /// CLI consumes it via `--resume <uuid>` / `--session <id>` / subcommand).
    /// The cause rides along because it selects the flag shape, not just the
    /// resume: `Regenerate` composes [`AgentProvider::resume_args_fork`] so
    /// the run branches off the transcript instead of extending it (issue
    /// #1550). Carrying it on the variant keeps "fork without a resume"
    /// unrepresentable at the one place that chooses the resume flags.
    Resume { id: &'a str, cause: ResumeCause },
    /// No session id — the harness self-assigns (e.g. mcode, kimi)
    /// and the orchestrator relies on the reader's PTY capture to learn it.
    None,
}

/// Inputs the harness needs to build its launch contribution. The resolver
/// has already applied the capability mask (issue #1149), so a `Some(model)`
/// here means "the harness accepts a model override AND the value is in its
/// vocabulary" — `prepare_launch` forwards it verbatim without re-consulting
/// capability flags. Pure (no I/O, no globals).
pub struct HarnessLaunchInput<'a> {
    /// Host platform the binary runs on. WSL meshes map to
    /// `Platform::Linux` so adapter recipes can use Linux-style flags.
    pub platform: Platform,
    /// Runtime spawn target (Windows vs WSL). WSL propagates additional
    /// env (see `apply_routing_env`); native Windows picks the WSL-aware
    /// Windows shell from each adapter's recipe.
    pub runtime: EnvType,
    /// Session id mode (assign / resume / none). The adapter decides
    /// whether its CLI accepts the id, the flag shape, or ignores it.
    pub session: SessionIdModeRef<'a>,
    /// Already-capability-masked resolved config. `Some(_)` values are
    /// safe to forward verbatim.
    pub config: &'a ResolvedAgentConfig,
    /// Prefill prompt text, if any. Only forwarded when the harness
    /// reports `supports_prefill`. The shared default trims and
    /// normalises CRLF before forwarding.
    pub prefill: Option<&'a str>,
    /// `true` when the parent mesh has its `sandbox` toggle on. The
    /// shared default appends the adapter's [`AgentProvider::sandbox_args`]
    /// contribution when this is set; the orchestrator's outer wrapper
    /// (`spawn_environment::wrap`) consults it independently for its
    /// own platform-level containment (macOS Seatbelt, Windows
    /// restricted-token). Issue #1287.
    pub sandbox: bool,
    /// Resolved routing facts; standalone recipe construction may omit them.
    pub routing: Option<&'a crate::agent::launch_routing::PreparedLaunchRouting>,
    /// Launch hooks need a node identity only for a real Buildmesh process.
    pub node_id: Option<i64>,
}

/// The environment policy a harness declares for its launch. Per-profile
/// backend env continues to flow through `PreparedLaunchRouting::Environment`.
/// `env_set` is for harness-specific child env injections.
#[derive(Default)]
pub struct HarnessEnvironmentPolicy {
    /// Keys to strip from the child env (e.g. Codex Proxy strips generic
    /// `OPENAI_API_KEY` / `OPENAI_BASE_URL` so the pairing-scoped
    /// credential reference is the only OpenAI auth path).
    pub env_remove: Vec<String>,
    /// Per-harness extra env to set on the child.
    pub env_set: Vec<(String, String)>,
}

/// The coherent launch contribution a harness produces for a single spawn:
/// the command the orchestrator will execute, the capability contract the
/// Spawn Menu / resolver / autopilot already advertised (so the consumer
/// can cross-check), and the env policy the spawn path applies after
/// routing. The capability descriptor travels with the recipe to make
/// the coherence testable: the same `AgentProvider` instance that
/// produced the recipe also produced the descriptor.
pub struct PreparedHarnessLaunch {
    pub recipe: SpawnRecipe,
    pub capabilities: crate::agent::capabilities::HarnessCapabilities,
    pub environment: HarnessEnvironmentPolicy,
}

/// Collapse `\r\n` and bare `\r` to `\n` in prefill text. GitHub issue/PR
/// bodies arrive with CRLF line endings; a bare `\r` reaching an agent's
/// TUI input on Windows is interpreted as Enter, submitting the prompt
/// after the first line. macOS / Linux (claude spawned directly) tolerate
/// CRLF, which is why this only bit Windows. Lives here so every adapter
/// path that consumes prefill goes through the same normalisation.
fn normalize_prefill_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Platform-aware prefill normalisation. Wraps [`normalize_prefill_newlines`]
/// with the shell-side concerns:
///
/// * On **Windows** (`WindowsShell::Cmd`) — `cmd.exe /c` treats a bare
///   newline inside a quoted argument as end-of-command, so all newlines
///   must be collapsed to single spaces. The CLI receives one argv
///   element with the full hand-off as a single line.
/// * On **macOS / Linux** (`WindowsShell::Direct`) — the binary is
///   spawned directly and argv elements preserve embedded newlines, so
///   the hand-off keeps its original line structure. CRLF is still
///   normalised to LF so cross-platform callers don't accidentally pass
///   `\r\n`.
///
/// Hoisted out of the Cline adapter (issue #1773 review) — flattening
/// was unconditional there, destroying multi-line prompts on Unix.
fn normalize_prefill_for_platform(platform: Platform, text: &str) -> String {
    match platform {
        Platform::Windows => text
            .split(['\n', '\r'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Platform::Macos | Platform::Linux => normalize_prefill_newlines(text),
    }
}

/// Delivery path for an automated node's first turn. A non-empty prompt uses
/// the harness's startup prefill when available and otherwise falls back to
/// the same two-phase PTY injection used for later automated turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InitialPromptDelivery {
    Fresh,
    Prefill,
    /// The CLI starts the first turn from its launch argument, without a PTY Enter.
    AutoSubmit,
    InjectAfterSpawn,
}

/// Select the transport for an automated first-turn prompt using the same
/// adapter that will prepare the process launch. The adapter owns the
/// harness-specific safety rule; the callers only choose how to deliver the
/// resulting prompt and therefore cannot grow provider-specific branches.
pub(crate) fn initial_prompt_delivery(spawn_option: &str, prompt: &str) -> InitialPromptDelivery {
    if prompt.trim().is_empty() {
        return InitialPromptDelivery::Fresh;
    }

    let adapter = crate::preferences::resolve_harness_provider(spawn_option).adapter();
    if adapter.descriptor().supports_prefill && !adapter.prefill_requires_pty(prompt) {
        if adapter.descriptor().prefill_auto_submits {
            InitialPromptDelivery::AutoSubmit
        } else {
            InitialPromptDelivery::Prefill
        }
    } else {
        InitialPromptDelivery::InjectAfterSpawn
    }
}

/// Shared default implementation of [`AgentProvider::prepare_launch`] for
/// the Claude-shaped recipe (every adapter that follows the base-recipe +
/// flag-arg pattern). Subcommand-style resume (Codex) returns options in
/// `base_args` and the session id in `trailing_args`; this helper layers
/// flags onto `base_args` and never relocates trailing positionals.
///
/// Consumes resolved configuration, session and routing inputs. Database
/// orchestration and OS wrapping remain outside adapter construction.
///
/// Behaviour pinned by the spawn command coherence tests
/// and the per-adapter tests under `adapters/`.
pub(super) fn default_prepare<A: AgentProvider + ?Sized>(
    adapter: &A,
    input: HarnessLaunchInput<'_>,
) -> PreparedHarnessLaunch {
    let capabilities = crate::agent::capabilities::capabilities_for(adapter);
    let base_recipe = adapter.spawn_recipe(input.platform, input.runtime);

    // The base recipe before session-id / override / prefill args are layered on.
    // Subcommand resume (Codex) returns options in `base_args` and the session
    // id in `trailing_args`. Layer flags onto `base_args` only — never pop
    // from the adapter's argv.
    // True when the resume path took the subcommand-style recipe
    // (`codex resume [OPTIONS] <id>`): its leading subcommand token must
    // stay at index 0 when the permission layer below splices options in.
    let mut subcommand_resume = false;
    let mut recipe = match input.session {
        SessionIdModeRef::Resume { id, cause } => {
            if let Some(resume_recipe) = adapter.spawn_recipe_for_resume(input.platform, id) {
                subcommand_resume = true;
                resume_recipe
            } else {
                let mut r = base_recipe;
                // Issue #1550 — a regenerate on the same harness reuses the
                // captured id but must not keep writing to the transcript it
                // starts from, so it composes the adapter's fork args. Every
                // other cause continues in place, unchanged.
                let args = match cause {
                    ResumeCause::Regenerate => adapter.resume_args_fork(id),
                    ResumeCause::Startup | ResumeCause::Explicit => adapter.resume_args(id),
                };
                if !args.is_empty() {
                    r.base_args.extend(args);
                }
                r
            }
        }
        SessionIdModeRef::Assign(id) => {
            let mut r = base_recipe;
            let args = adapter.session_assign_args(id);
            if !args.is_empty() {
                r.base_args.extend(args);
            }
            r
        }
        SessionIdModeRef::None => base_recipe,
    };

    // Issue #2151 — permission mode. The bare base recipe carries no
    // approval flags; the effective mode contributes them here, at the
    // FRONT of `base_args` — exactly where the base recipe used to carry
    // them — so today's argv order is preserved byte-for-byte and no
    // harness re-parses a changed argv. `None` (no layer supplied a mode)
    // launches with the harness's unattended default (today's flags), so
    // existing meshes keep working until the person changes the setting.
    //
    // The one exception is a subcommand-style resume (`codex resume
    // [OPTIONS] <id>` via `spawn_recipe_for_resume`): the leading
    // subcommand token stays at index 0 and the permission options
    // follow it, matching the old `[resume, <base flags...>]`
    // composition. (A flag-style resume such as muse's `resume <uuid>`
    // pair takes the front like everything else, reproducing the old
    // `[flags..., resume, <uuid>]` order.)
    //
    // Defence in depth, mirroring the model/effort branches below: the
    // shared fallback re-checks the resolved id against the adapter's
    // own mode list, so a caller that bypasses the resolver cannot
    // smuggle an unknown mode into argv (`permission_args` is
    // prompt-equivalent for unknown ids anyway).
    {
        let modes = &capabilities.permission_modes;
        if let Some(option) = crate::agent::capabilities::effective_permission_mode(
            modes,
            capabilities.default_permission_mode.as_deref(),
            input.config.permission_mode.as_deref(),
        ) {
            let args = adapter.permission_args(&option.id);
            if !args.is_empty() {
                let insert_at = usize::from(subcommand_resume);
                recipe.base_args.splice(insert_at..insert_at, args);
            }
        }
    }

    // The resolver already applied the capability mask; `Some` here means
    // the harness accepts this control AND the value is in its vocabulary
    // (issue #1149 acceptance criteria 6, 7, 9). The mask is the single
    // guarantee that unsupported values never reach a harness process.
    //
    // Defence in depth (issue #1179): the helper also re-asserts the
    // capability descriptor on the forward, so a caller that bypasses
    // the resolver (e.g. a future internal call site, a test) cannot
    // smuggle a model arg into a harness that advertised no support.
    // The capability and the recipe come from the same adapter — they
    // cannot disagree by construction.
    if capabilities.supports_model_override && !adapter.model_override_via_session_api() {
        if let Some(model) = input.config.model.as_deref().filter(|s| !s.is_empty()) {
            recipe.base_args.extend(adapter.model_args(model));
        }
    }
    if !matches!(capabilities.effort_control, EffortControlKind::None) {
        if let Some(effort) = input.config.effort.as_deref().filter(|s| !s.is_empty()) {
            recipe.base_args.extend(adapter.effort_args(effort));
        }
    }
    // Issue #1358: forward the per-step `extra_args` verbatim through
    // the adapter's tokenise helper. Mirrors the model / effort
    // branches above — capability-masked at the resolver (no
    // harness-side check needed here) and only forwarded when the
    // capability descriptor advertises support. Placed between
    // `effort_args` and `sandbox_args` so the semantic-then-positional
    // layer order survives: model / effort / extras are user-supplied,
    // sandbox is mesh-driven, prefill is the trailing positional.
    if capabilities.supports_extra_args {
        if let Some(extra) = input.config.extra_args.as_deref().filter(|s| !s.is_empty()) {
            // Gracefully degrade rather than crashing the spawn worker
            // thread on a user typo (unclosed quote, dangling escape).
            // PR #1362 round 2 review: a `panic!` here is a fatal runtime
            // failure for a circuit-author-authored value. We log and
            // fall back to whitespace splitting so the spawn still
            // proceeds (the user's other args land correctly; only the
            // malformed token group is dropped).
            match adapter.extra_args_args(extra) {
                Ok(args) => recipe.base_args.extend(args),
                Err(e) => {
                    tracing::warn!(
                        "extra_args tokenisation failed ({}); falling back to whitespace split for {:?}",
                        e, extra
                    );
                    recipe
                        .base_args
                        .extend(extra.split_whitespace().map(String::from));
                }
            }
        }
    }

    // Issue #1287 — adapter-level sandbox flag. When the parent mesh
    // has its `sandbox` toggle on, append the adapter's declared
    // sandbox contribution (e.g. Antigravity's `--sandbox`). Layered
    // on top of the orchestrator's outer containment wrapper (macOS
    // Seatbelt / Windows restricted-token) — the two are independent
    // layers: the outer wrapper confines the filesystem, the adapter
    // flag confines the agent's own terminal-side operations.
    if input.sandbox {
        let sandbox = adapter.sandbox_args();
        if !sandbox.is_empty() {
            recipe.base_args.extend(sandbox);
        }
    }

    if capabilities.supports_prefill {
        if let Some(text) = input.prefill.filter(|s| !s.is_empty()) {
            // Platform-aware: `cmd.exe /c` flattens to single space on Windows;
            // direct spawn on macOS/Linux preserves line structure.
            let normalized = normalize_prefill_for_platform(input.platform, text);
            let args = adapter.prefill_args(&normalized);
            // Prefill after a trailing session id is also positional
            // (`codex resume [OPTIONS] <id> [PROMPT]`). Flag-shaped prefill
            // (`--prefill text`) stays on `base_args` when there are no
            // trailing positionals.
            if recipe.trailing_args.is_empty() {
                recipe.base_args.extend(args);
            } else {
                recipe.trailing_args.extend(args);
            }
        }
    }

    if let Some(node_id) = input.node_id {
        recipe
            .base_args
            .extend(adapter.launch_hook_args(node_id, input.runtime));
    }
    let mut environment = HarnessEnvironmentPolicy::default();
    if let Some(crate::agent::launch_routing::PreparedLaunchRouting::Environment {
        values, ..
    }) = input.routing
    {
        environment.env_set.extend(values.iter().cloned());
    }

    PreparedHarnessLaunch {
        recipe,
        capabilities,
        environment,
    }
}

/// Assert `args` contains `flag` immediately followed by `value` (no
/// other arg interleaved). Used by the per-adapter recipe pins (grok
/// `--model`, kimi `-m`) to catch the specific failure mode the
/// table-driven `capability_recipe_coherence` cannot — silent
/// short↔long flag swaps that the adapter would silently follow.
#[cfg(test)]
pub(crate) fn assert_flag_followed_by_value(args: &[String], flag: &str, value: &str) {
    let idx = args
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("prepared recipe must contain {flag} flag; got args = {args:?}"));
    assert_eq!(
        args.get(idx + 1).map(String::as_str),
        Some(value),
        "prepared recipe must put {value:?} immediately after {flag}; got args = {args:?}"
    );
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
