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

// In scope for the `deserialize_opt_string` helper below.
use serde::Deserialize;

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

/// Tolerates both an *absent* key (rescued by `#[serde(default)]` on the
/// field) and a present-but-`null` value. The latter is the gotcha: serde
/// does NOT invoke `Default` when the key is present with a JSON `null`, so a
/// per-field `deserialize_with` is required for any field that may arrive as
/// `null`. `body` is the one such field today, on both [`Issue`] and
/// [`PullRequest`] — GitHub emits `body: null` on an issue or pull request
/// opened without a description (issues #1210 and #2333).
///
/// Private to this module's subtree, where both wire shapes live: one rule for
/// `Issue` and `PullRequest` means a bodyless issue and a bodyless pull request
/// render as the same empty description.
fn deserialize_opt_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}
