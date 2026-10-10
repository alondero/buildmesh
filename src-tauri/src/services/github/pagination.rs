//! One RFC 8288 `Link`-header paginator for every GitHub *list* read (issue #1528).
//!
//! Before this module each read path hand-rolled its own paging: some took a
//! single `per_page=100` response and presented it as the whole repository,
//! others looped `?page=N` with no cap at all. Both failure modes are silent —
//! the caller cannot tell "page 1 of 9" from "that is everything". This module
//! is the single place that decides how far a read walks and how it reports
//! what it got.
//!
//! ## The contract
//!
//! 1. **Follow `rel="next"`** ([`next_page_url`]), never a hand-rolled page
//!    counter. The `Link` header is the only cursor GitHub promises is
//!    consistent across endpoints and across interleaved writes.
//! 2. **A failed page fails the whole read.** `?` on the request means a
//!    500/403 on page 2 returns `Err` — page 1 is *never* handed back as a
//!    successful read. Truncation is reported as data (see
//!    [`GitHubPageCompleteness`]), never as a silent success.
//! 3. **Bounded.** At most [`PaginationPolicy::max_pages`] requests, checked
//!    *before* the request goes out, so a pathological feed cannot burn the
//!    rate limit or park the blocking-pool thread.
//! 4. **Cancellable.** A cancelled read stops before its next request and
//!    reports [`GitHubIncompleteReason::Cancelled`].
//! 5. **Ordered and deduplicated.** Items keep server order across the
//!    concatenation; an identity repeated across pages (possible when an item
//!    is created between two requests) is kept once, at its first position.
//!
//! ## Why `max_pages` is 10
//!
//! The default policy is 10 pages × `per_page=100` = 1,000 items, which is
//! exactly GitHub's search ceiling ([`SEARCH_RESULT_CEILING`]). Past that the
//! service cannot return more anyway, so a larger cap would only spend
//! requests to re-derive the same bound. Real repositories are far below it
//! (buildmesh's own tracker is a few hundred), and the probe surfaces
//! truncation rather than hiding it.

use std::collections::HashSet;
use std::hash::Hash;

use reqwest::header::{ACCEPT, AUTHORIZATION, LINK, USER_AGENT};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::sync::{rest_failure, GitHubClient, GitHubError};

/// Items requested per page. GitHub's maximum for REST list endpoints.
pub const DEFAULT_PER_PAGE: usize = 100;

/// Hard ceiling on pages fetched by one read. See the module doc comment for
/// why it is exactly GitHub's search ceiling rather than an arbitrary number.
pub const DEFAULT_MAX_PAGES: usize = 10;

/// GitHub's search API never returns more than 1,000 matches for a query,
/// regardless of pagination. A search envelope whose `total_count` exceeds
/// this can never be fully read, so it is reported incomplete rather than
/// presented as the whole answer.
pub const SEARCH_RESULT_CEILING: i64 = 1000;

/// Page-size and page-count budget for one read. Tests and any read with a
/// tighter budget construct their own; production uses [`Default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaginationPolicy {
    pub per_page: usize,
    pub max_pages: usize,
}

impl Default for PaginationPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl PaginationPolicy {
    /// The production budget. `const` so a module-level cap can be derived
    /// from the same source of truth rather than restating "1000" elsewhere.
    pub const DEFAULT: Self = Self {
        per_page: DEFAULT_PER_PAGE,
        max_pages: DEFAULT_MAX_PAGES,
    };

    /// Largest number of items this policy can return.
    pub const fn max_items(&self) -> usize {
        self.per_page.saturating_mul(self.max_pages)
    }
}

/// Why a read returned fewer items than GitHub reports. `None` on
/// [`GitHubPageCompleteness::incomplete_reason`] means the read is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "GitHubIncompleteReason.ts")]
pub enum GitHubIncompleteReason {
    /// The page budget ran out with pages still pending.
    SafetyCap,
    /// GitHub's search API cannot return more than [`SEARCH_RESULT_CEILING`]
    /// matches, so this feed can never be read in full.
    SearchCeiling,
    /// GitHub itself set `incomplete_results: true` on the search envelope —
    /// the upstream index timed out before matching every item.
    UpstreamIncomplete,
    /// The caller cancelled the read before it finished.
    Cancelled,
}

/// What the UI needs to avoid presenting a truncated read as the whole
/// repository. Attached to every multi-page read so "showing first N" is
/// visible instead of inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "GitHubPageCompleteness.ts")]
pub struct GitHubPageCompleteness {
    /// Items actually returned after concatenation and deduplication.
    #[ts(as = "i32")]
    pub returned: i64,
    /// HTTP pages fetched. Zero only for a read cancelled before it started.
    #[ts(as = "i32")]
    pub pages_fetched: i64,
    /// `true` only when every page GitHub advertised was consumed.
    pub complete: bool,
    /// `None` exactly when `complete` is `true`.
    pub incomplete_reason: Option<GitHubIncompleteReason>,
    /// GitHub's own `total_count`, when the endpoint reports one (search
    /// envelopes do; REST list endpoints do not). Lets the UI say "100 of
    /// 1,240" rather than an unexplained "truncated".
    #[ts(as = "Option<i32>")]
    pub reported_total: Option<i64>,
}

impl GitHubPageCompleteness {
    /// Complete read of a REST list (no endpoint-reported total).
    pub(super) fn rest(returned: usize, pages_fetched: usize) -> Self {
        Self {
            returned: returned as i64,
            pages_fetched: pages_fetched as i64,
            complete: true,
            incomplete_reason: None,
            reported_total: None,
        }
    }
}

/// One page-walked read: the items plus how much of the feed they cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub completeness: GitHubPageCompleteness,
}

impl<T> Page<T> {
    /// Discard the completeness metadata. Only for callers that cannot act on
    /// it and whose own contract already fails loudly on a short read
    /// (reconciliation ingest — see [`require_complete_read`]).
    pub fn into_items(self) -> Vec<T> {
        self.items
    }
}

/// Reconciliation ingest (Autopilot / Circuits) must never act on a partial
/// read: a silently shortened trigger feed means a labelled issue or PR on
/// page 2 never gets a run, with no evidence anything went wrong. This turns
/// any incomplete page-walked read into an error the poll pass logs and
/// isolates, which is the "fail visibly" half of the contract.
///
/// Read-only consumers (the probes) must NOT use this — they show the data
/// plus [`GitHubPageCompleteness`] so the user can see and judge the gap.
pub(crate) fn require_complete_read<T>(page: Page<T>, what: &str) -> Result<Vec<T>, GitHubError> {
    if page.completeness.complete {
        return Ok(page.items);
    }
    let reason = page
        .completeness
        .incomplete_reason
        .expect("complete=false always carries a reason");
    Err(GitHubError::Incomplete {
        what: what.to_string(),
        reason,
        returned: page.completeness.returned,
        reported_total: page.completeness.reported_total,
    })
}

/// Extract the `rel="next"` target from an RFC 8288 `Link` header value.
///
/// GitHub emits the header in every paged REST response, e.g.
/// `<https://api.github.com/repositories/1/issues?page=2>; rel="next", <...?page=9>; rel="last"`.
/// A comma is only a separator *outside* `<…>` and quoted strings, so the
/// scan below tracks both rather than splitting on `,` — a label or branch
/// containing one would otherwise split an entry in half.
///
/// `pub(super)` so the test module can pin the parser against real header
/// values independently of any HTTP round trip.
pub(super) fn next_page_url(header: &str) -> Option<String> {
    for (target, params) in split_link_entries(header) {
        if param_rel_is_next(&params) {
            return Some(target);
        }
    }
    None
}

/// Split a `Link` header value into `(target, parameters)` pairs.
fn split_link_entries(header: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = header.chars().collect();
    let mut entries = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i] != '<' {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        i += 1; // consume '<'
        let target_start = i;
        while i < chars.len() && chars[i] != '>' {
            i += 1;
        }
        let target: String = chars[target_start..i].iter().collect();
        if i < chars.len() {
            i += 1; // consume '>'
        }
        // Parameters run to the next entry's target. Two things end this run:
        // an unquoted `<` (the next entry's target), and an unquoted comma
        // that is immediately followed by one — that comma is the RFC 8288
        // entry separator, and leaving it attached to the parameters would
        // corrupt the value (`rel="next",` instead of `rel="next"`). A comma
        // inside quotes is part of a value and is kept.
        let params_start = i;
        let mut in_quotes = false;
        while i < chars.len() {
            match chars[i] {
                '"' => in_quotes = !in_quotes,
                '<' if !in_quotes => break,
                ',' if !in_quotes && opens_next_entry(&chars, i + 1) => break,
                _ => {}
            }
            i += 1;
        }
        let params: String = chars[params_start..i].iter().collect();
        entries.push((target.trim().to_string(), params));
    }
    entries
}

/// `true` when the next non-whitespace character at `from` is `<`, i.e. a new
/// link-value-target starts there.
fn opens_next_entry(chars: &[char], from: usize) -> bool {
    chars
        .get(from..)
        .and_then(|rest| rest.iter().find(|c| !c.is_whitespace()))
        == Some(&'<')
}

/// `true` when an entry's parameter list contains a `rel` whose value list
/// includes `next`. The parameter name is matched case-insensitively and the
/// value may be quoted or bare, and may carry several relation types
/// (`rel="next last"`) — all three shapes show up in GitHub responses and in
/// the proxies that sit in front of them.
fn param_rel_is_next(params: &str) -> bool {
    params
        .split(';')
        .filter_map(|param| param.trim().split_once('='))
        .any(|(name, value)| {
            if !name.trim().eq_ignore_ascii_case("rel") {
                return false;
            }
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            value
                .split_whitespace()
                .any(|relation| relation.eq_ignore_ascii_case("next"))
        })
}

/// One parsed page, before dedup. `total` / `upstream_incomplete` are `None` /
/// `false` for REST list endpoints, which report neither.
struct ParsedPage<T> {
    items: Vec<T>,
    total: Option<i64>,
    upstream_incomplete: bool,
}

impl GitHubClient {
    /// Walk every page of a REST list endpoint, following `rel="next"`.
    ///
    /// `first_url` must already carry `per_page`; the adapter only follows the
    /// links GitHub returns. `key_of` supplies the item identity used to drop
    /// duplicates across pages.
    pub(crate) fn get_all_pages<T, K, F>(
        &self,
        first_url: &str,
        policy: &PaginationPolicy,
        key_of: F,
    ) -> Result<Page<T>, GitHubError>
    where
        T: DeserializeOwned,
        K: Eq + Hash,
        F: Fn(&T) -> K,
    {
        collect_pages(self, first_url, policy, key_of, |resp| {
            let items: Vec<T> = resp.json()?;
            Ok(ParsedPage {
                items,
                total: None,
                upstream_incomplete: false,
            })
        })
    }

    /// Walk every page of a search envelope (`/search/issues`).
    ///
    /// Same walk as [`Self::get_all_pages`], plus the two completeness signals
    /// only search provides: `total_count` and GitHub's own
    /// `incomplete_results` flag. Exhausting the `Link` chain is *not* proof of
    /// completeness here — GitHub stops the chain at 1,000 matches while still
    /// reporting a larger `total_count`, so the shortfall is recorded rather
    /// than presented as the whole answer.
    pub(crate) fn search_all_pages<T, K, F>(
        &self,
        query_url: &str,
        policy: &PaginationPolicy,
        key_of: F,
    ) -> Result<Page<T>, GitHubError>
    where
        T: DeserializeOwned,
        K: Eq + Hash,
        F: Fn(&T) -> K,
    {
        // `total_count` and `incomplete_results` are always present on a
        // search envelope, and `Option` fields default to `None` when a key is
        // absent, so a partial payload still parses.
        #[derive(Deserialize)]
        struct SearchEnvelope<T> {
            items: Vec<T>,
            total_count: Option<i64>,
            incomplete_results: Option<bool>,
        }

        collect_pages(self, query_url, policy, key_of, |resp| {
            let envelope: SearchEnvelope<T> = resp.json()?;
            Ok(ParsedPage {
                items: envelope.items,
                total: envelope.total_count,
                upstream_incomplete: envelope.incomplete_results.unwrap_or(false),
            })
        })
    }
}

/// The shared page walk behind [`GitHubClient::get_all_pages`] and
/// [`GitHubClient::search_all_pages`]. `parse` turns one still-unread response
/// into that endpoint's page shape — it is handed the response (not a parsed
/// `Value`) so each endpoint deserialises in a single pass.
fn collect_pages<T, K, F, P>(
    client: &GitHubClient,
    first_url: &str,
    policy: &PaginationPolicy,
    key_of: F,
    parse: P,
) -> Result<Page<T>, GitHubError>
where
    T: DeserializeOwned,
    K: Eq + Hash,
    F: Fn(&T) -> K,
    P: Fn(reqwest::blocking::Response) -> Result<ParsedPage<T>, GitHubError>,
{
    let mut items: Vec<T> = Vec::new();
    let mut seen: HashSet<K> = HashSet::new();
    let mut next_url = Some(first_url.to_string());
    let mut pages_fetched: usize = 0;
    let mut reason: Option<GitHubIncompleteReason> = None;
    let mut reported_total: Option<i64> = None;
    let mut upstream_incomplete = false;

    while let Some(url) = next_url {
        // Cancellation and the page budget are both checked *before* the
        // request leaves, so a cancelled or capped read costs nothing.
        if client.is_cancelled() {
            reason = Some(GitHubIncompleteReason::Cancelled);
            break;
        }
        if pages_fetched >= policy.max_pages {
            reason = Some(GitHubIncompleteReason::SafetyCap);
            break;
        }

        let resp = client.authorized_get(&url)?;
        let status = resp.status();
        if !status.is_success() {
            // Contract point 2: a failed page fails the whole read. Returning
            // page 1 here is exactly the "silently incomplete" bug of #1528.
            let body = resp.text().unwrap_or_default();
            return Err(rest_failure(status, body));
        }
        // Read the header before the body consumes the response.
        let link = resp
            .headers()
            .get(LINK)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        pages_fetched += 1;
        let parsed = parse(resp)?;
        if parsed.total.is_some() {
            reported_total = parsed.total;
        }
        upstream_incomplete |= parsed.upstream_incomplete;
        for item in parsed.items {
            // First occurrence wins, so ordering stays server order even when
            // an item straddles a page boundary.
            if seen.insert(key_of(&item)) {
                items.push(item);
            }
        }

        next_url = link.as_deref().and_then(next_page_url);
    }

    // An explicit stop (cap / cancellation) outranks the endpoint's own
    // signals; otherwise fall back to what GitHub told us about itself.
    let returned = items.len() as i64;
    if reason.is_none() {
        reason = if upstream_incomplete {
            Some(GitHubIncompleteReason::UpstreamIncomplete)
        } else if reported_total.is_some_and(|total| returned < total) {
            // The `Link` chain ran out before GitHub's own `total_count`. When
            // that total is past the search ceiling the service *cannot* serve
            // the rest — that is the documented cap, not a bug. Below it, the
            // chain ending early is an unexplained upstream shortfall, and it
            // must not be dressed up as a known limit.
            if reported_total.is_some_and(|total| total > SEARCH_RESULT_CEILING) {
                Some(GitHubIncompleteReason::SearchCeiling)
            } else {
                Some(GitHubIncompleteReason::UpstreamIncomplete)
            }
        } else {
            None
        };
    }

    let complete = reason.is_none();
    let mut completeness = GitHubPageCompleteness::rest(items.len(), pages_fetched);
    completeness.reported_total = reported_total;
    if !complete {
        completeness.complete = false;
        completeness.incomplete_reason = reason;
    }
    Ok(Page {
        items,
        completeness,
    })
}

/// The authenticated GET every GitHub read shares. Lives here so the paginated
/// and hand-written paths cannot drift apart on auth headers or the client
/// timeout (which `build_http_client` already bounds).
impl GitHubClient {
    pub(super) fn authorized_get(
        &self,
        url: &str,
    ) -> Result<reqwest::blocking::Response, GitHubError> {
        self.client
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "buildmesh")
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .map_err(|e| {
                tracing::debug!("GitHub GET {} failed: {}", url, e);
                GitHubError::Http(e)
            })
    }
}
