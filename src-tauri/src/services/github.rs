//! GitHub API behind the Issues and Pull Requests probes.
//!
//! Command handlers keep calling the names re-exported here. Resource code
//! lives in the submodule for the probe it serves:
//!
//! - [`issues`] — issue listing, Blocked-by parsing, trigger-label flags.
//! - [`prs`] — pull-request listing, merge strategy, contributor data.
//! - [`pagination`] — the one RFC 8288 `Link`-header page walk every list read
//!   uses, its page budget and cancellation, and the completeness metadata that
//!   makes a truncated read visible instead of silent (issue #1528).
//! - [`sync`] — host token, HTTP timeouts, and rate-limit errors. TTL and
//!   live-over-cache helpers live here as tested policy for a future
//!   snapshot store; live list methods fetch every time.

pub mod issues;
pub mod pagination;
pub mod prs;
pub mod sync;

#[cfg(test)]
mod label_tests;
#[cfg(test)]
mod pagination_tests;

pub use issues::{parse_blocked_by, Issue};
pub use pagination::GitHubPageCompleteness;
pub use prs::{CollaboratorPermission, CreatePrRequest, PullRequest, PullRequestMergeState};
pub use sync::{parse_clone_input, parse_owner_repo, CloneTarget, GitHubClient, GitHubError};

/// Fake GitHub server used by command-layer tests. The implementation lives
/// with the pull-request client; this re-export keeps `services::github::tests`
/// stable.
#[cfg(test)]
pub(crate) mod tests {
    pub(crate) use super::prs::tests::{fake_node, fake_server, Scripted};
}
