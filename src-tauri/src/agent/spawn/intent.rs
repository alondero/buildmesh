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
    /// Custom spawn-prompt template resolved once, at construction, from
    /// the stored `issue_spawn_prompt` preference (production sites read
    /// it via `preferences::issue_spawn_prompt()`). `None` selects the
    /// built-in default. Carrying the template inside the intent - rather
    /// than re-reading global state at every `initial_prompt()` call -
    /// keeps the desktop draft, the background launch, and the Autopilot
    /// watcher byte-identical by construction: they all render the same
    /// carried value.
    pub(crate) template: Option<String>,
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
    /// Custom spawn-prompt template resolved once, at construction, from
    /// the stored `pr_spawn_prompt` preference. Same carried-value
    /// contract as [`IssueContext::template`].
    pub(crate) template: Option<String>,
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
    Prompt { text: String },
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
    /// | `Prompt { text }`                  | `Some(text)` verbatim                           |
    /// | `Resume { .. }`                    | `None`                                          |
    /// | `Issue(context)` w/ title          | `Some("Please work on ... #N — title\n<url>")`  |
    /// | `Issue(context)` blank title       | `Some("Please work on ... #N\n<url>")`          |
    /// | `Issue(context)` w/ `template`     | `Some(render_issue_spawn_prompt(template, ...))` |
    /// | `PullRequest(context)`             | `Some("Review PR #N\n<shared review policy>\n<url>")` |
    /// | `PullRequest(context)` w/ `template` | `Some(render_pr_spawn_prompt(template, ...))` |
    /// | `Handover { selected_text }`       | `Some(selected_text)` verbatim                  |
    /// | `Loop { initial_prompt }`          | `Some(initial_prompt)` verbatim                 |
    ///
    /// The `template` is carried inside the context (resolved once, at
    /// construction, from the stored preference), so every consumer that
    /// holds the same intent renders the same string however many times
    /// it asks - byte-identity by construction, not by convention.
    pub(crate) fn initial_prompt(&self) -> Option<InitialPrompt> {
        match self {
            Self::Fresh | Self::Resume { .. } => None,
            Self::Prompt { text } => Some(InitialPrompt(text.clone())),
            Self::Issue(context) => {
                let prompt = match context.template.as_deref() {
                    Some(template) => render_issue_spawn_prompt(
                        template,
                        &context.owner,
                        &context.repo,
                        context.number,
                        &context.title,
                    ),
                    None => format_issue_prefill(
                        &context.owner,
                        &context.repo,
                        context.number,
                        &context.title,
                    ),
                };
                Some(InitialPrompt(prompt))
            }
            Self::PullRequest(context) => {
                let prompt = match context.template.as_deref() {
                    Some(template) => render_pr_spawn_prompt(
                        template,
                        &context.owner,
                        &context.repo,
                        context.number,
                    ),
                    None => format_pull_request_prefill(
                        &context.owner,
                        &context.repo,
                        context.number,
                    ),
                };
                Some(InitialPrompt(prompt))
            }
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
    /// Optional per-step wall-clock budget in seconds (#1219). `None`
    /// = the orchestrator uses its own default. Circuit-level
    /// enforcement reads the graph node in
    /// `services::circuit_worker::observe_waits`; process-level
    /// enforcement (cancelling stuck spawns) is a follow-up — this
    /// field is the data shape for the launch carrier. `Some(0)` is
    /// treated as "absent" by the resolver rather than "expire
    /// immediately", mirroring the whitespace-only collapse rule above.
    pub(crate) timeout_seconds: Option<u32>,
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
    /// Opt into the generic durable node lease used by a caller that owns a
    /// recoverable lifecycle. Ordinary desktop/HTTP spawns leave this off so
    /// the low-level orchestrator does not query circuit recovery state.
    pub(crate) lifecycle_lease: bool,
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
            lifecycle_lease: false,
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

    /// Opt this request into the generic durable node lifecycle lease. Circuit
    /// execution uses it to fence cleanup against a concurrent resume.
    pub(crate) fn with_lifecycle_lease(mut self) -> Self {
        self.lifecycle_lease = true;
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

/// Format the GitHub-PR prefill. Single source of truth (issue #1180, #1561);
/// see [`format_issue_prefill`] for the parallel doc.
pub(crate) fn format_pull_request_prefill(
    owner: &str,
    repo: &str,
    number: i64,
) -> String {
    let url = format!("https://github.com/{owner}/{repo}/pull/{number}");
    format!(
        "Review PR #{number}\n{}\n{url}",
        crate::review_contract::REVIEW_POLICY
    )
}

/// Default template for the Issues-probe spawn prompt, shown in Settings
/// as the default and used whenever the user has no custom template
/// stored. Rendered via [`render_issue_spawn_prompt`]; rendering this
/// constant is byte-identical to [`format_issue_prefill`] (pinned by
/// `default_issue_template_matches_legacy_prefill` below).
pub const DEFAULT_ISSUE_SPAWN_TEMPLATE: &str =
    "Please work on GitHub issue #{{number}}{{title_suffix}}\n{{url}}";

/// Default template for the PR-probe spawn prompt, shown in Settings as
/// the default and used whenever the user has no custom template stored.
/// Rendered via [`render_pr_spawn_prompt`]; rendering this constant is
/// byte-identical to [`format_pull_request_prefill`] (pinned by
/// `default_pr_template_matches_legacy_prefill` below).
pub const DEFAULT_PR_SPAWN_TEMPLATE: &str = "Review PR #{{number}}\n{{policy}}\n{{url}}";

/// Substitute `{{name}}` placeholders in one left-to-right pass.
/// Each placeholder is resolved against `vars` exactly once and the
/// substituted value is emitted verbatim - never re-scanned - so a value
/// that itself contains placeholder-shaped text (e.g. an issue titled
/// `Fix {{url}} parsing`) cannot trigger secondary expansion. Unknown
/// names and unterminated `{{` sequences pass through untouched, so a
/// typo stays visible in the prompt instead of silently vanishing.
fn render_template(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = &after[..end];
                match vars.iter().find(|(key, _)| *key == name) {
                    Some((_, value)) => out.push_str(value),
                    None => out.push_str(&rest[start..start + 2 + end + 2]),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Render an Issues-probe spawn template. Placeholders:
/// `{{number}}`, `{{title}}` (trimmed, may be empty), `{{title_suffix}}`
/// (`" - <title>"` with an em dash, or empty when the title is blank, so
/// a custom template built on the default keeps the no-dangling-dash
/// contract), `{{url}}`, `{{owner}}`, `{{repo}}`. Unknown `{{...}}`
/// placeholders are left in place.
pub(crate) fn render_issue_spawn_prompt(
    template: &str,
    owner: &str,
    repo: &str,
    number: i64,
    title: &str,
) -> String {
    let title = title.trim();
    let title_suffix = if title.is_empty() {
        String::new()
    } else {
        format!(" \u{2014} {title}")
    };
    let url = format!("https://github.com/{owner}/{repo}/issues/{number}");
    let number = number.to_string();
    render_template(
        template,
        &[
            ("number", number.as_str()),
            ("title_suffix", title_suffix.as_str()),
            ("title", title),
            ("url", url.as_str()),
            ("owner", owner),
            ("repo", repo),
        ],
    )
}

/// Render a PR-probe spawn template. Placeholders: `{{number}}`,
/// `{{url}}`, `{{owner}}`, `{{repo}}`, `{{policy}}` (the shared review
/// policy from [`crate::review_contract::REVIEW_POLICY`]). Unknown
/// `{{...}}` placeholders are left in place.
pub(crate) fn render_pr_spawn_prompt(
    template: &str,
    owner: &str,
    repo: &str,
    number: i64,
) -> String {
    let url = format!("https://github.com/{owner}/{repo}/pull/{number}");
    let number = number.to_string();
    render_template(
        template,
        &[
            ("number", number.as_str()),
            ("policy", crate::review_contract::REVIEW_POLICY),
            ("url", url.as_str()),
            ("owner", owner),
            ("repo", repo),
        ],
    )
}

/// Render an Issues-probe spawn template when the canonical issue URL is
/// already available from a trigger payload (circuit `issue.*` namespace)
/// instead of owner/repo parts. Same placeholders as
/// [`render_issue_spawn_prompt`]; `{{owner}}` / `{{repo}}` are derived by
/// parsing the canonical `https://github.com/{owner}/{repo}/issues/{n}`
/// URL and fall back to empty strings when the URL does not parse (so a
/// custom template still renders rather than failing the run).
pub(crate) fn render_issue_spawn_prompt_with_url(
    template: &str,
    number: i64,
    title: &str,
    url: &str,
) -> String {
    let (owner, repo) = parse_github_issue_url(url);
    let title = title.trim();
    let title_suffix = if title.is_empty() {
        String::new()
    } else {
        format!(" \u{2014} {title}")
    };
    let number = number.to_string();
    render_template(
        template,
        &[
            ("number", number.as_str()),
            ("title_suffix", title_suffix.as_str()),
            ("title", title),
            ("url", url),
            ("owner", owner.as_str()),
            ("repo", repo.as_str()),
        ],
    )
}

/// Split a canonical `https://github.com/{owner}/{repo}/...` URL into its
/// owner/repo parts. Returns empty strings when the URL does not have the
/// expected shape; the caller decides how to handle the fallback.
fn parse_github_issue_url(url: &str) -> (String, String) {
    const PREFIX: &str = "https://github.com/";
    let Some(path) = url.strip_prefix(PREFIX) else {
        return (String::new(), String::new());
    };
    let mut segments = path.split('/');
    match (segments.next(), segments.next()) {
        (Some(owner), Some(repo)) if !owner.is_empty() && !repo.is_empty() => {
            (owner.to_string(), repo.to_string())
        }
        _ => (String::new(), String::new()),
    }
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
            template: None,
        });

        assert_eq!(
            intent.initial_prompt().as_ref().map(InitialPrompt::as_str),
            Some(
                "Please work on GitHub issue #247 — Deepen spawn pipeline\n\
https://github.com/alondero/buildmesh/issues/247"
            )
        );
    }

    /// Pin the PR prefill contract (issue #1180 AC #3, #1561): the shared
    /// review policy is used for both interactive and circuit reviews.
    #[test]
    fn pull_request_prefill_uses_canonical_pull_url_and_shared_review_policy() {
        let intent = SpawnIntent::PullRequest(PullRequestContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 420,
            template: None,
        });

        let expected = format!(
            "Review PR #420\n{}\nhttps://github.com/alondero/buildmesh/pull/420",
            crate::review_contract::REVIEW_POLICY
        );
        assert_eq!(
            intent.initial_prompt().as_ref().map(InitialPrompt::as_str),
            Some(expected.as_str())
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

    #[test]
    fn mobile_idea_is_the_initial_prompt_verbatim() {
        let intent = SpawnIntent::Prompt { text: "Fix the mobile view\nKeep terminal access.".into() };
        assert_eq!(intent.initial_prompt().unwrap().as_str(), "Fix the mobile view\nKeep terminal access.");
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
            template: None,
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
            template: None,
        });
        // Same shape as the empty-title case.
        let expected_issue = SpawnIntent::Issue(IssueContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 1,
            title: String::new(),
            template: None,
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
            template: None,
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

    /// Rendering the default issue template must be byte-identical to the
    /// legacy prefill - for titled, empty, and whitespace-only titles - so
    /// "no custom template stored" can never drift from the historical
    /// wording.
    #[test]
    fn default_issue_template_matches_legacy_prefill() {
        for title in ["Deepen spawn pipeline", "", "   \t  "] {
            assert_eq!(
                render_issue_spawn_prompt(
                    DEFAULT_ISSUE_SPAWN_TEMPLATE,
                    "alondero",
                    "buildmesh",
                    247,
                    title
                ),
                format_issue_prefill("alondero", "buildmesh", 247, title),
                "title {title:?} must render identically"
            );
        }
    }

    /// Same byte-identity contract for the PR default template.
    #[test]
    fn default_pr_template_matches_legacy_prefill() {
        assert_eq!(
            render_pr_spawn_prompt(DEFAULT_PR_SPAWN_TEMPLATE, "alondero", "buildmesh", 420),
            format_pull_request_prefill("alondero", "buildmesh", 420),
        );
    }

    /// Custom issue templates substitute every placeholder; the title is
    /// trimmed before substitution.
    #[test]
    fn custom_issue_template_substitutes_placeholders() {
        let out = render_issue_spawn_prompt(
            "Working {{owner}}/{{repo}}#{{number}}: {{title}} ({{url}})",
            "alondero",
            "buildmesh",
            7,
            "  Fix it  ",
        );
        assert_eq!(
            out,
            "Working alondero/buildmesh#7: Fix it \
             (https://github.com/alondero/buildmesh/issues/7)"
        );
    }

    /// A blank title empties both `{{title}}` and `{{title_suffix}}`, so a
    /// custom template built on the default never leaves a dangling dash.
    #[test]
    fn custom_issue_template_blank_title_empties_title_suffix() {
        let out = render_issue_spawn_prompt(
            DEFAULT_ISSUE_SPAWN_TEMPLATE,
            "alondero",
            "buildmesh",
            7,
            "   ",
        );
        assert_eq!(
            out,
            "Please work on GitHub issue #7\nhttps://github.com/alondero/buildmesh/issues/7"
        );
    }

    /// Custom PR templates get the shared review policy plus the PR URL.
    #[test]
    fn custom_pr_template_substitutes_policy_and_url() {
        let out = render_pr_spawn_prompt(
            "Look at {{owner}}/{{repo}}#{{number}}: {{url}}\n{{policy}}",
            "alondero",
            "buildmesh",
            9,
        );
        assert!(
            out.contains("https://github.com/alondero/buildmesh/pull/9"),
            "PR URL must be substituted: {out:?}"
        );
        assert!(
            out.contains(crate::review_contract::REVIEW_POLICY),
            "shared review policy must be substituted"
        );
    }

    /// Placeholders the renderer does not know are left in place rather
    /// than silently deleted, so a typo stays visible in the prompt.
    #[test]
    fn unknown_placeholders_are_left_in_place() {
        let out = render_issue_spawn_prompt(
            "#{{number}} {{bogus}}",
            "alondero",
            "buildmesh",
            1,
            "t",
        );
        assert_eq!(out, "#1 {{bogus}}");
    }

    /// A substituted value is emitted verbatim, never re-scanned: an
    /// issue titled `Fix {{url}} parsing` must reach the prompt with its
    /// braces intact instead of being expanded to the issue URL by the
    /// later `{{url}}` pass. Regression for the cascading-`.replace()`
    /// secondary expansion.
    #[test]
    fn substituted_values_are_never_rescanned() {
        let out = render_issue_spawn_prompt(
            DEFAULT_ISSUE_SPAWN_TEMPLATE,
            "alondero",
            "buildmesh",
            7,
            "Fix {{url}} and {{number}} parsing",
        );
        assert_eq!(
            out,
            "Please work on GitHub issue #7 \u{2014} Fix {{url}} and {{number}} parsing\n\
             https://github.com/alondero/buildmesh/issues/7"
        );
    }

    /// Unterminated `{{` sequences pass through untouched rather than
    /// swallowing the rest of the template.
    #[test]
    fn unterminated_placeholder_passes_through() {
        let out = render_issue_spawn_prompt(
            "#{{number}} {{oops",
            "alondero",
            "buildmesh",
            1,
            "t",
        );
        assert_eq!(out, "#1 {{oops");
    }

    /// A carried custom template renders through `initial_prompt()` - the
    /// same intent value the desktop draft, the background launch, and
    /// the Autopilot watcher all share, so they agree by construction.
    #[test]
    fn carried_issue_template_renders_through_initial_prompt() {
        let intent = SpawnIntent::Issue(IssueContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 3,
            title: "Hi".into(),
            template: Some("Custom #{{number}}: {{title}}".into()),
        });
        assert_eq!(
            intent.initial_prompt().map(|p| p.into_string()),
            Some("Custom #3: Hi".to_string()),
        );
    }

    /// `template: None` selects the built-in default wording.
    #[test]
    fn missing_template_falls_back_to_default() {
        let intent = SpawnIntent::Issue(IssueContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 3,
            title: "Hi".into(),
            template: None,
        });
        assert_eq!(
            intent.initial_prompt().map(|p| p.into_string()),
            Some(
                "Please work on GitHub issue #3 \u{2014} Hi\n\
                 https://github.com/alondero/buildmesh/issues/3"
                    .to_string()
            ),
        );
    }

    /// A carried custom PR template renders the PR URL through
    /// `initial_prompt()`.
    #[test]
    fn carried_pr_template_renders_through_initial_prompt() {
        let intent = SpawnIntent::PullRequest(PullRequestContext {
            owner: "alondero".into(),
            repo: "buildmesh".into(),
            number: 11,
            template: Some("Look at {{url}}".into()),
        });
        assert_eq!(
            intent.initial_prompt().map(|p| p.into_string()),
            Some("Look at https://github.com/alondero/buildmesh/pull/11".to_string()),
        );
    }

    /// The URL-variant renderer parses a canonical issue URL for
    /// `{{owner}}` / `{{repo}}` so circuit `issue.*` prefills honour the
    /// same custom template as Probe spawns.
    #[test]
    fn with_url_renderer_derives_owner_and_repo() {
        let out = render_issue_spawn_prompt_with_url(
            "{{owner}}/{{repo}}#{{number}}: {{title}} ({{url}})",
            9,
            "Fix it",
            "https://github.com/alondero/buildmesh/issues/9",
        );
        assert_eq!(
            out,
            "alondero/buildmesh#9: Fix it (https://github.com/alondero/buildmesh/issues/9)"
        );
    }

    /// A non-canonical URL degrades to empty owner/repo rather than
    /// failing the render - the number, title, and URL still land.
    #[test]
    fn with_url_renderer_tolerates_unparseable_url() {
        let out = render_issue_spawn_prompt_with_url("[{{owner}}/{{repo}}] #{{number}} {{url}}", 1, "t", "u");
        assert_eq!(out, "[/] #1 u");
    }

    /// Rendering the default template through the URL variant matches the
    /// legacy `format_issue_prefill_with_url` byte-for-byte.
    #[test]
    fn with_url_default_template_matches_legacy_prefill() {
        let url = "https://github.com/alondero/buildmesh/issues/5";
        assert_eq!(
            render_issue_spawn_prompt_with_url(DEFAULT_ISSUE_SPAWN_TEMPLATE, 5, "T", url),
            format_issue_prefill_with_url(5, "T", url),
        );
    }
}
