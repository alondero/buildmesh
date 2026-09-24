//! GitHub API behind the Issues and Pull Requests probes.
//!
//! Command handlers keep calling the names re-exported here. Resource code
//! lives in the submodule for the probe it serves:
//!
//! - [`issues`] — issue listing, Blocked-by parsing, trigger-label flags.
//! - [`prs`] — pull-request listing, merge strategy, contributor data.
//! - [`sync`] — host token, HTTP timeouts, and rate-limit errors. TTL and
//!   live-over-cache helpers live here as tested policy for a future
//!   snapshot store; live list methods fetch every time.

pub mod issues;
pub mod prs;
pub mod sync;

pub use issues::{parse_blocked_by, Issue};
pub use prs::{CollaboratorPermission, CreatePrRequest, PullRequest};
pub use sync::{parse_owner_repo, GitHubClient, GitHubError};

/// Fake GitHub server used by command-layer tests. The implementation lives
/// with the pull-request client; this re-export keeps `services::github::tests`
/// stable.
#[cfg(test)]
pub(crate) mod tests {
    pub(crate) use super::prs::tests::{fake_node, fake_server, Scripted};
}
