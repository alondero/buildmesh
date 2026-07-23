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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnRequest {
    pub(crate) node_id: i64,
    pub(crate) intent: SpawnIntent,
    pub(crate) terminal_size: TerminalSize,
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
}
