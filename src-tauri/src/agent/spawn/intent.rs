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
pub(crate) struct IssueContext {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) number: i64,
    pub(crate) title: String,
}


/// Context required to spawn an agent on a GitHub pull request.
/// Does not carry `title` because the PR prefill is persona-driven and
/// intentionally independent of the PR title. The PR title is used
/// separately for node naming (`session_naming::pr_node_name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PullRequestContext {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) number: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeCause {
    Startup,
    Explicit,
}

/// Worktree policy supplied by a caller that knows the spawn's purpose.
/// Most spawns inherit the mesh setting; issue-driven circuit runs opt into a
/// real branched worktree without pretending to be a legacy `autopilot_runs`
/// row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum WorktreePolicy {
    #[default]
    RespectMesh,
    ForceBranched,
}

/// The authoritative initial prompt a [`SpawnIntent`] will hand to the
/// harness (issue #1180). Built once, at the spawn seam, so the desktop
/// draft, the background launch, and the Autopilot watcher all derive
/// from the same source — there's no longer a free function to
/// accidentally diverge from the live spawn path.
///
/// `InitialPrompt` is a newtype (not a plain `String`) so consumers can't
/// accidentally concatenate, slice, or re-format it; the only ways out
/// are `as_str()` (borrow) and `into_string()` (consume, for transport
/// paths that need an owned `String` like the desktop `IssueNodeDraft`
/// wire shape or the Autopilot watcher's prefill buffer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitialPrompt(String);

impl InitialPrompt {
    /// Borrow the prompt text without consuming it. Used by the spawn
    /// pipeline (`spawn_with_intent`) and the watcher marker helper
    /// (`marker_hint_for_prefill`) where a `&str` is all that's needed.
    ///
    /// `#[allow(dead_code)]`: shipped as part of #1180 alongside the doc'd
    /// callers (`spawn_with_intent`, `marker_hint_for_prefill`) that haven't
    /// landed yet — the docstring is the contract. When those call sites
    /// wire up, this allow drops. The companion `into_string` has the
    /// same shape — its only call site is the intent's own test module —
    /// so the same escape hatch could land there in a follow-up; it's not
    /// applied today because the failure isn't biting.
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper, returning the owned `String`. Used by the
    /// desktop draft response (`IssueNodeDraft.prefill`) and the
    /// Autopilot watcher's prefill buffer, both of which need ownership
    /// to outlive their `SpawnIntent` builder.
    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for InitialPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnIntent {
    Fresh,
    Issue(IssueContext),
    PullRequest(PullRequestContext),
    Handover { selected_text: String },
    Loop { initial_prompt: String },
    Resume { cause: ResumeCause },
}

impl SpawnIntent {
    /// Build the authoritative initial prompt once, at the spawn seam
    /// (issue #1180, #1561). Every consumer — desktop draft, background launch,
    /// Autopilot watcher — routes through this method instead of
    /// recomputing from a free function. The truth table:
    ///
    /// | Variant                            | Result                                          |
    /// |------------------------------------|-------------------------------------------------|
    /// | `Fresh`                            | `None`                                          |
    /// | `Resume { .. }`                    | `None`                                          |
    /// | `Issue(context)` w/ title          | `Some("Please work on ... #N — title\n<url>")`  |
    /// | `Issue(context)` blank title       | `Some("Please work on ... #N\n<url>")`          |
    /// | `PullRequest(context)`             | `Some("Review PR #N as a grumpy...\n<url>")`    |
    /// | `Handover { selected_text }`       | `Some(selected_text)` verbatim                  |
    /// | `Loop { initial_prompt }`          | `Some(initial_prompt)` verbatim                 |
    pub(crate) fn initial_prompt(&self) -> Option<InitialPrompt> {
        match self {
            Self::Fresh | Self::Resume { .. } => None,
            Self::Issue(context) => Some(InitialPrompt(format_issue_prefill(
                &context.owner,
                &context.repo,
                context.number,
                &context.title,
            ))),
            Self::PullRequest(context) => Some(InitialPrompt(format_pull_request_prefill(
                &context.owner,
                &context.repo,
                context.number,
            ))),
            Self::Handover { selected_text } => Some(InitialPrompt(selected_text.clone())),
            Self::Loop { initial_prompt } => Some(InitialPrompt(initial_prompt.clone())),
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
    /// Optional verbatim CLI flag string this one spawn should forward,
    /// overriding nothing in the cascade (no mesh / application layer
    /// carries per-spawn flags — this is the only layer of supply for
    /// circuit-author-supplied flags, issue #1358). Whitespace-only
    /// collapses to absent; an empty additional-args list is equivalent
    /// to "no override". Capability-masked downstream — a harness whose
    /// capability descriptor advertises `supports_extra_args = false`
    /// silently drops the value at the resolver rather than forwarding
    /// it as a synthetic flag.
    pub(crate) extra_args: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnRequest {
    pub(crate) node_id: i64,
    pub(crate) intent: SpawnIntent,
    pub(crate) terminal_size: TerminalSize,
    /// Cascade layer-1 overrides (issue #1155). Empty / whitespace-only
    /// values are normalised to absent before reaching the resolver.
    pub(crate) explicit: ExplicitSpawnOverrides,
    /// Worktree strategy override for this spawn. Kept on the request rather
    /// than the persisted Agent Node so a circuit can use the shared spawn
    /// orchestrator without adding mode-specific DB state.
    pub(crate) worktree_policy: WorktreePolicy,
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
    pub(crate) fn new(node_id: i64, intent: SpawnIntent, terminal_size: TerminalSize) -> Self {
        Self {
            node_id,
            intent,
            terminal_size,
            explicit: ExplicitSpawnOverrides::default(),
            worktree_policy: WorktreePolicy::RespectMesh,
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

    /// Force a branched worktree for issue-driven circuit work while keeping
    /// the ordinary mesh policy for all existing callers.
    pub(crate) fn with_worktree_policy(mut self, policy: WorktreePolicy) -> Self {
        self.worktree_policy = policy;
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) enum SpawnOutcome {
    Started(AgentNode),
    AlreadyActive(AgentNode),
    Skipped(AgentNode),
}

/// Format the GitHub-issue prefill. Single source of truth (issue #1180):
/// every caller (desktop draft, background launch, Autopilot watcher)
/// routes through [`SpawnIntent::initial_prompt`] which calls this
/// helper. Public-within-crate so `commands::agent::IssueNodeDraft` tests
/// can construct equivalent `InitialPrompt` values for comparison.
pub(crate) fn format_issue_prefill(owner: &str, repo: &str, number: i64, title: &str) -> String {
    let url = format!("https://github.com/{owner}/{repo}/issues/{number}");
    format_issue_prefill_with_url(number, title, &url)
}

/// Format the GitHub-issue prefill when the canonical issue URL is already
/// available from a trigger payload. Both the desktop spawn seam and circuit
/// context use this formatter so the wording cannot drift between transports.
pub(crate) fn format_issue_prefill_with_url(number: i64, title: &str, url: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        format!("Please work on GitHub issue #{number}\n{url}")
    } else {
        format!("Please work on GitHub issue #{number} — {title}\n{url}")
    }
}

/// Canonical persona instruction for PR reviews spawned from the PR probe.
///
/// NOTE on divergence from [`crate::autopilot::circuit::model::CircuitGraph::PR_REVIEW_PROMPT`]:
/// The probe prefill is for an interactive desktop session spawned by a human user:
/// it is capitalized ("Review PR #..."), appends the canonical PR URL for context,
/// and omits the automated headless directive ("Add the review comments to the PR as a comment")
/// because the human user guides the interactive session.
/// `CircuitGraph::PR_REVIEW_PROMPT` is a headless background circuit template with
/// `{{pr.number}}` that instructs an autonomous reviewer bot to post findings directly
/// to GitHub without human supervision.
const PR_REVIEW_PERSONA: &str =
    "as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture";

/// Format the GitHub-PR prefill. Single source of truth (issue #1180, #1561);
/// see [`format_issue_prefill`] for the parallel doc.
pub(crate) fn format_pull_request_prefill(
    owner: &str,
    repo: &str,
    number: i64,
) -> String {
    let url = format!("https://github.com/{owner}/{repo}/pull/{number}");
    format!("Review PR #{number} {PR_REVIEW_PERSONA}\n{url}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the issue prefill contract (issue #1180 AC #1):
    /// `Please work on GitHub issue #N — Title\n<canonical URL>` with the
    /// title trimmed. A future change here would silently shift what every
    /// transport surfaces — desktop draft, background launch, and the
    /// Autopilot watcher — so the wording is locked to this exact shape.
    #[test]
    fn issue_prefill_uses_canonical_github_url_and_trimmed_title() {
        let intent = SpawnIntent::Issue(IssueContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 247,
            title: "  Deepen spawn pipeline  ".into(),
        });

        assert_eq!(
            intent.initial_prompt().as_ref().map(InitialPrompt::as_str),
            Some(
                "Please work on GitHub issue #247 — Deepen spawn pipeline\n\
https://github.com/alondero/buildmesh/issues/247"
            )
        );
    }

    /// Pin the PR prefill contract (issue #1180 AC #3, #1561):
    /// `Review PR #N as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture\n<url>`.
    #[test]
    fn pull_request_prefill_uses_canonical_pull_url_and_grumpy_engineer_prompt() {
        let intent = SpawnIntent::PullRequest(PullRequestContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 420,
        });

        assert_eq!(
            intent.initial_prompt().as_ref().map(InitialPrompt::as_str),
            Some(
                "Review PR #420 as a grumpy senior engineer who is obsessed with writing the right code, clean code, and having the right architecture\n\
https://github.com/alondero/buildmesh/pull/420"
            )
        );
    }

    /// `Fresh` and `Resume` carry no initial prompt — the harness boots
    /// clean and either sits on the resume marker or waits for user
    /// input (issue #1180 AC #7).
    #[test]
    fn fresh_and_resume_have_no_initial_prompt() {
        assert_eq!(SpawnIntent::Fresh.initial_prompt(), None);
        assert_eq!(
            SpawnIntent::Resume {
                cause: ResumeCause::Startup
            }
            .initial_prompt(),
            None
        );
        assert_eq!(
            SpawnIntent::Resume {
                cause: ResumeCause::Explicit
            }
            .initial_prompt(),
            None
        );
    }

    /// `Handover` passes the user's selection through verbatim — it's
    /// already complete text, no URL or number needs appending (issue
    /// #1180 AC #5). A round-trip via `into_string()` proves the wrapper
    /// owns the value, not borrows.
    #[test]
    fn handover_initial_prompt_passes_selected_text_verbatim() {
        let intent = SpawnIntent::Handover {
            selected_text: "fix the\n  whitespace trim".into(),
        };
        let prompt = intent.initial_prompt().expect("handover has a prompt");
        assert_eq!(prompt.as_str(), "fix the\n  whitespace trim");
        assert_eq!(prompt.into_string(), "fix the\n  whitespace trim");
    }

    /// `Loop` uses the configured initial prompt verbatim — same rationale
    /// as Handover, the loop prefill is the user-authored contract (issue
    /// #1180 AC #6).
    #[test]
    fn loop_initial_prompt_passes_configured_text_verbatim() {
        let intent = SpawnIntent::Loop {
            initial_prompt: "iterate on the failing tests".into(),
        };
        let prompt = intent.initial_prompt().expect("loop has a prompt");
        assert_eq!(prompt.as_str(), "iterate on the failing tests");
    }

    /// `InitialPrompt` is a value type — clone + equality + display all
    /// must work the way a plain `String` does, otherwise downstream
    /// helpers that take `&str` (the watcher marker, the spawn recipe
    /// prefill) would need bespoke adapters.
    #[test]
    fn initial_prompt_supports_clone_eq_and_display() {
        let p = InitialPrompt("hello".into());
        assert_eq!(p.clone(), p);
        assert_eq!(format!("{p}"), "hello");
    }

    /// Pin the AC #2 truth-table entry: an issue with a blank title
    /// degrades to `Please work on GitHub issue #N\n<url>` (no dangling
    /// em-dash artifact). Same shape as the trimmed-title case, minus
    /// the title segment.
    #[test]
    fn issue_prefill_with_empty_title_falls_back_to_number_only() {
        let intent = SpawnIntent::Issue(IssueContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 7,
            title: String::new(),
        });
        assert_eq!(
            intent.initial_prompt().as_ref().map(InitialPrompt::as_str),
            Some(
                "Please work on GitHub issue #7\n\
https://github.com/alondero/buildmesh/issues/7"
            )
        );
    }

    /// Whitespace-only title is normalised to empty for the purposes of
    /// the title-trim branch — the prompt reads like the empty-title
    /// case so no dangling em-dash reaches the harness.
    #[test]
    fn issue_prefill_treats_whitespace_title_as_empty() {
        let issue = SpawnIntent::Issue(IssueContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 1,
            title: "   \t  ".into(),
        });
        // Same shape as the empty-title case.
        let expected_issue = SpawnIntent::Issue(IssueContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 1,
            title: String::new(),
        })
        .initial_prompt();
        assert_eq!(issue.initial_prompt(), expected_issue);
    }

    /// Titles with double quotes pass through verbatim — the prefill
    /// uses an em-dash separator, not surrounding quotes, so there is
    /// nothing to escape. Regression for issue #420 — the old helper
    /// was the canonical place where this property held; after #1180
    /// the contract moved here.
    #[test]
    fn issue_prefill_preserves_quotes_in_title_verbatim() {
        let intent = SpawnIntent::Issue(IssueContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 42,
            title: "Fix the \"weird\" race in spawn".into(),
        });
        let prefill = intent
            .initial_prompt()
            .expect("issue always has a prompt")
            .into_string();
        assert!(
            prefill.contains("Fix the \"weird\" race in spawn"),
            "title quotes must reach the LLM verbatim: {:?}",
            prefill
        );
        assert!(
            !prefill.contains('\\'),
            "title must NOT carry backslash escapes: {:?}",
            prefill
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
