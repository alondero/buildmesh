//! Domain intent types for starting an Agent Node process.
//!
//! Callers describe why a node is being started. Provider capabilities,
//! session identifiers, and the persisted node remain implementation details
//! of the parent `spawn` module.

use crate::models::AgentNode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalSize {
    pub(crate) rows: u16,
    pub(crate) cols: u16,
}

/// Default terminal dimensions (24×80) for the per-spawn UI affordance.
/// Centralised here so the constructor at [`SpawnRequest::new`] and every
/// call site that doesn't need caller-supplied dimensions share the same
/// source of truth (issue #1157). Pinned by the unit test
/// `terminal_size_default_is_24x80`.
impl Default for TerminalSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitHubWorkContext {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) number: i64,
    pub(crate) title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeCause {
    Startup,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnIntent {
    Fresh,
    Issue(GitHubWorkContext),
    PullRequest(GitHubWorkContext),
    Handover { selected_text: String },
    Loop { initial_prompt: String },
    Resume { cause: ResumeCause },
}

impl SpawnIntent {
    /// Build the initial prompt once, at the spawn seam, instead of requiring
    /// each transport to know the provider's prefill rules.
    pub(crate) fn prefill(&self) -> Option<String> {
        match self {
            Self::Fresh | Self::Resume { .. } => None,
            Self::Issue(context) => Some(format_issue_prefill(
                &context.owner,
                &context.repo,
                context.number,
                &context.title,
            )),
            Self::PullRequest(context) => Some(format_pull_request_prefill(
                &context.owner,
                &context.repo,
                context.number,
                &context.title,
            )),
            Self::Handover { selected_text } => Some(selected_text.clone()),
            Self::Loop { initial_prompt } => Some(initial_prompt.clone()),
        }
    }
}

/// Cascade layer-1 overrides supplied by the caller of a single spawn
/// (issue #1155). Highest precedence in the spawn-config cascade:
///
/// 1. **Explicit Agent Node spawn argument** — values the caller passed for
///    this one spawn (e.g. an autopilot-side override, a future
///    `--model <x> --effort <y>` CLI flag, a mobile HTTP request body).
/// 2. Mesh row value (`meshes.model` / `meshes.effort`).
/// 3. Application-level default (`preferences::harness_defaults`).
/// 4. Harness native fallback.
///
/// Both fields are optional and independent — a caller can pin only the
/// model, only the effort, both, or neither. Empty / whitespace-only
/// strings are collapsed to `None` at the helper that builds the resolver
/// inputs (`spawn::cascade_inputs_for`) so the cascade falls through to
/// the next layer, matching every other layer's normalisation rule
/// (issue #1148 AC #32).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExplicitSpawnOverrides {
    /// Optional model id this one spawn should use, overriding the mesh
    /// row and the application default. `None` or whitespace-only
    /// collapses to absent.
    pub(crate) model: Option<String>,
    /// Optional effort / reasoning value this one spawn should use,
    /// overriding the mesh row and the application default. `None` or
    /// whitespace-only collapses to absent.
    pub(crate) effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnRequest {
    pub(crate) node_id: i64,
    pub(crate) intent: SpawnIntent,
    pub(crate) terminal_size: TerminalSize,
    /// Cascade layer-1 overrides (issue #1155). Empty / whitespace-only
    /// values are normalised to absent before reaching the resolver.
    pub(crate) explicit: ExplicitSpawnOverrides,
}

impl SpawnRequest {
    /// Build a `SpawnRequest` with empty layer-1 overrides (issue #1157).
    /// Centralises the boilerplate every transport site was duplicating
    /// (the `TerminalSize { rows: 24, cols: 80 }` literal and the
    /// `explicit: Default::default()` line) so future layer-1 wiring —
    /// any transport that actually has a per-spawn override to apply —
    /// reaches for [`Self::with_explicit`] rather than re-declaring the
    /// struct literal. The contract — `explicit` is `Default::default()`
    /// on construction — is regression-pinned by
    /// `spawn_request_new_sets_explicit_default` in `spawn::tests`.
    pub(crate) fn new(
        node_id: i64,
        intent: SpawnIntent,
        terminal_size: TerminalSize,
    ) -> Self {
        Self {
            node_id,
            intent,
            terminal_size,
            explicit: ExplicitSpawnOverrides::default(),
        }
    }

    /// Builder for the cascade layer-1 (explicit) override slot. The
    /// consuming-self signature lets call sites chain
    /// `SpawnRequest::new(...).with_explicit(...)` without an
    /// intermediate `let mut`. No current call site uses this — every
    /// spawn today inherits the mesh row + application default through
    /// the cascade — but it documents the future-facing API for the
    /// layer-1 feature and is exercised by the integration test
    /// `spawn_request_explicit_wins_at_resolver` (issue #1155 AC #4).
    #[allow(dead_code)] // no production call site yet — exercised by `mod tests`
    pub(crate) fn with_explicit(mut self, explicit: ExplicitSpawnOverrides) -> Self {
        self.explicit = explicit;
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) enum SpawnOutcome {
    Started(AgentNode),
    AlreadyActive(AgentNode),
    Skipped(AgentNode),
}

fn format_issue_prefill(owner: &str, repo: &str, number: i64, title: &str) -> String {
    let url = format!("https://github.com/{owner}/{repo}/issues/{number}");
    let title = title.trim();
    if title.is_empty() {
        format!("Please work on GitHub issue #{number}\n{url}")
    } else {
        format!("Please work on GitHub issue #{number} — {title}\n{url}")
    }
}

fn format_pull_request_prefill(owner: &str, repo: &str, number: i64, title: &str) -> String {
    let url = format!("https://github.com/{owner}/{repo}/pull/{number}");
    let title = title.trim();
    if title.is_empty() {
        format!("Please review pull request #{number}\n{url}")
    } else {
        format!("Please review pull request #{number} — {title}\n{url}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_prefill_uses_canonical_github_url_and_trimmed_title() {
        let intent = SpawnIntent::Issue(GitHubWorkContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 247,
            title: "  Deepen spawn pipeline  ".into(),
        });

        assert_eq!(
            intent.prefill().as_deref(),
            Some(
                "Please work on GitHub issue #247 — Deepen spawn pipeline\n\
https://github.com/alondero/buildmesh/issues/247"
            )
        );
    }

    /// Pin the default terminal dimensions (issue #1157). The constructor
    /// at [`SpawnRequest::new`] and every call site that doesn't need
    /// caller-supplied dimensions rely on this being 24×80 — a future
    /// change would silently shift every manual/auto-spawn's initial
    /// terminal render.
    #[test]
    fn terminal_size_default_is_24x80() {
        assert_eq!(TerminalSize::default(), TerminalSize { rows: 24, cols: 80 });
    }
}
