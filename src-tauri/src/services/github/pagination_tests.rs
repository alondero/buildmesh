//! Loopback tests for the `Link`-header paginator (issue #1528).
//!
//! Every walk test drives the *production* paginator against a real socket, so
//! the assertions are about HTTP behaviour — which page is requested next, what
//! happens when one fails, whether a cancelled read stops — not about a
//! re-implementation of the paging logic.
//!
//! Thread-lifecycle note (matching the existing `fake_server` in `prs.rs`):
//! the fake serves exactly the scripted pages and never blocks the test on
//! `join`. A client that stops early (cancellation / cap) leaves the server
//! parked in `accept()`, which is harmless — the thread dies with the process,
//! and the request counter (asserted before the test ends) is the real
//! evidence that no extra request went out.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::pagination::{
    next_page_url, GitHubIncompleteReason, PaginationPolicy, SEARCH_RESULT_CEILING,
};
use super::{GitHubClient, GitHubError};

/// One scripted page. The `Link` header is generated from the fake's own bound
/// address, so a test never has to know its port.
struct Step {
    /// Request path without the page query (e.g. `/search/issues`).
    base_path: String,
    /// 1-based page number this step answers. `0` means the request path is
    /// taken verbatim from `base_path` (an endpoint that already carries a
    /// query string).
    page: u32,
    status: u16,
    body: String,
    /// Page number to advertise as `rel="next"` after this step. `None` = last
    /// page, and the server sends no `Link` header at all (as GitHub does).
    next_page: Option<u32>,
    /// Path the `Link` header points at, when it differs from the path this
    /// step answers. GitHub rewrites the query wholesale (`?page=2`), so a
    /// follow-up's URL is not always derivable from the first request's.
    link_path: Option<String>,
}

impl Step {
    fn new(base_path: &str, page: u32, body: String) -> Self {
        Self {
            base_path: base_path.to_string(),
            page,
            status: 200,
            body,
            next_page: None,
            link_path: None,
        }
    }

    /// A step whose request path is taken verbatim — for endpoints that
    /// already carry a query string (`/search/issues?q=…`), where appending
    /// `?page=N` would not be the URL the client sends.
    fn at(request_path: &str, body: String) -> Self {
        Self::new(request_path, 0, body)
    }

    /// Point this step's `Link` header at an explicit path.
    fn linking_to(mut self, path: &str) -> Self {
        self.link_path = Some(path.to_string());
        self
    }

    /// A step that links onward, so the test only has to list pages in order.
    fn linking(base_path: &str, page: u32, body: String, next_page: u32) -> Self {
        Self {
            next_page: Some(next_page),
            ..Self::new(base_path, page, body)
        }
    }

    fn failing(mut self, status: u16, message: &str) -> Self {
        self.status = status;
        self.body = serde_json::json!({ "message": message }).to_string();
        self
    }
}

/// A loopback GitHub bound to an ephemeral port.
struct Fake {
    base_url: String,
    listener: Option<TcpListener>,
    requests: Arc<AtomicUsize>,
    steps: Vec<Step>,
}

impl Fake {
    /// Bind the listener now and serve later, so a test can construct a
    /// [`GitHubClient`] pointed at `base_url` (and hand it to the serving
    /// thread) before any request arrives.
    fn bind(steps: Vec<Step>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        Self {
            base_url: format!("http://{addr}"),
            listener: Some(listener),
            requests: Arc::new(AtomicUsize::new(0)),
            steps,
        }
    }

    fn url(&self, base_path: &str, page: u32) -> String {
        format!("{}{base_path}?page={page}", self.base_url)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Serve the script on a background thread. `before_response(index)` runs
    /// after the request is read and before the response is written, which is
    /// what lets a test cancel the client mid-walk deterministically.
    fn spawn<F>(mut self, mut before_response: F) -> Self
    where
        F: FnMut(usize) + Send + 'static,
    {
        let listener = self.listener.take().expect("bind before spawn");
        let steps = std::mem::take(&mut self.steps);
        let requests = Arc::clone(&self.requests);
        let base_url = self.base_url.clone();
        std::thread::spawn(move || {
            for (index, step) in steps.into_iter().enumerate() {
                let (mut sock, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(_) => return,
                };
                requests.fetch_add(1, Ordering::SeqCst);
                sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .expect("read timeout");
                let mut reader = BufReader::new(sock.try_clone().expect("clone"));
                let mut request_line = String::new();
                reader
                    .read_line(&mut request_line)
                    .expect("read request line");
                let got_path = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let expected_path = if step.page == 0 {
                    step.base_path.clone()
                } else {
                    format!("{}?page={}", step.base_path, step.page)
                };
                assert_eq!(
                    got_path, expected_path,
                    "client requested an unexpected page (script expected {expected_path})"
                );
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("read header");
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some(v) = trimmed
                        .strip_prefix("content-length:")
                        .or_else(|| trimmed.strip_prefix("Content-Length:"))
                    {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                if content_length > 0 {
                    let mut body = vec![0u8; content_length];
                    std::io::Read::read_exact(&mut reader, &mut body).expect("read body");
                }

                before_response(index);

                let link_header = match step.next_page {
                    Some(next) => {
                        let target = match step.link_path.as_deref() {
                            Some(path) => path.to_string(),
                            None => format!("{}?page={}", step.base_path, next),
                        };
                        format!("Link: <{base_url}{target}>; rel=\"next\"\r\n")
                    }
                    None => String::new(),
                };
                let body = step.body;
                let response = format!(
                    "HTTP/1.1 {} Status\r\nContent-Type: application/json\r\n{link_header}\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    step.status,
                    body.len(),
                    body
                );
                let _ = sock.write_all(response.as_bytes());
                let _ = sock.flush();
            }
        });
        self
    }

    /// Bind and serve with no hook.
    fn serve(steps: Vec<Step>) -> Self {
        Self::bind(steps).spawn(|_| {})
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        // Close the listener so a server thread parked in `accept()` wakes and
        // exits instead of outliving the suite.
        if let Some(listener) = self.listener.take() {
            drop(listener);
        }
    }
}

/// Minimal item shape the tests deserialise into.
#[derive(Debug, serde::Deserialize)]
struct Row {
    number: i64,
}

/// `{"number": N}` rows, in the order given.
fn rows(numbers: &[i64]) -> String {
    let items: Vec<serde_json::Value> = numbers
        .iter()
        .map(|n| serde_json::json!({ "number": n }))
        .collect();
    serde_json::json!(items).to_string()
}

/// A `/search/issues` envelope carrying `items` and a `total_count`.
fn search_envelope(numbers: &[i64], total: i64) -> String {
    let items: Vec<serde_json::Value> = numbers
        .iter()
        .map(|n| serde_json::json!({ "number": n }))
        .collect();
    serde_json::json!({
        "total_count": total,
        "incomplete_results": false,
        "items": items,
    })
    .to_string()
}

/// Issue-shaped items. `services::github::Issue` requires `number` and
/// `title`; the rest of the wire fields are `#[serde(default)]`, so these
/// two are enough to exercise the real ingest deserialiser.
fn issue_rows(numbers: &[i64]) -> String {
    let items: Vec<serde_json::Value> = numbers
        .iter()
        .map(|n| serde_json::json!({ "number": n, "title": format!("Issue {n}") }))
        .collect();
    serde_json::json!(items).to_string()
}

fn issue_envelope(numbers: &[i64], total: i64) -> String {
    let items: Vec<serde_json::Value> = numbers
        .iter()
        .map(|n| serde_json::json!({ "number": n, "title": format!("Issue {n}") }))
        .collect();
    serde_json::json!({
        "total_count": total,
        "incomplete_results": false,
        "items": items,
    })
    .to_string()
}

fn numbers(items: &[Row]) -> Vec<i64> {
    items.iter().map(|r| r.number).collect()
}

const ISSUES: &str = "/repos/acme/demo/issues";
const SEARCH: &str = "/search/issues";
const FILES: &str = "/repos/acme/demo/pulls/7/files";

// ---------------------------------------------------------------------------
// Link header parsing (pure, no sockets).
// ---------------------------------------------------------------------------

#[test]
fn next_link_selects_the_next_relation_from_a_real_github_header() {
    let header = "<https://api.github.com/repositories/1/issues?page=2>; rel=\"next\", \
                  <https://api.github.com/repositories/1/issues?page=9>; rel=\"last\"";
    assert_eq!(
        next_page_url(header).as_deref(),
        Some("https://api.github.com/repositories/1/issues?page=2")
    );
}

#[test]
fn next_link_is_absent_on_a_single_page_response() {
    // GitHub omits `Link` entirely on the last page, and a header carrying only
    // `prev`/`last` must not be mistaken for a next page.
    assert_eq!(next_page_url(""), None);
    assert_eq!(next_page_url("<https://api.github.com/x?page=1>; rel=\"first\""), None);
    assert_eq!(
        next_page_url(
            "<https://api.github.com/x?page=1>; rel=\"prev\", \
             <https://api.github.com/x?page=9>; rel=\"last\""
        ),
        None
    );
}

#[test]
fn next_link_tolerates_unquoted_spaced_and_multi_valued_rel_forms() {
    for header in [
        "<https://api.github.com/x?page=2>; rel=next",
        "<https://api.github.com/x?page=2>; rel = \"next\"",
        "<https://api.github.com/x?page=2>; REL=\"NEXT\"",
        "<https://api.github.com/x?page=2>; rel=\"next last\"",
    ] {
        assert_eq!(
            next_page_url(header).as_deref(),
            Some("https://api.github.com/x?page=2"),
            "failed to read the next link from {header}"
        );
    }
}

#[test]
fn next_link_does_not_split_on_a_comma_inside_the_url() {
    // A branch or label can legitimately contain a comma; splitting the header
    // on `,` would truncate the target and follow a wrong URL.
    let header = "<https://api.github.com/x?q=a,b&page=2>; rel=\"next\", \
                  <https://api.github.com/x?q=a,b&page=9>; rel=\"last\"";
    assert_eq!(
        next_page_url(header).as_deref(),
        Some("https://api.github.com/x?q=a,b&page=2")
    );
}

// ---------------------------------------------------------------------------
// The walk, over a loopback server.
// ---------------------------------------------------------------------------

#[test]
fn three_pages_arrive_once_each_and_in_server_order() {
    let fake = Fake::serve(vec![
        Step::linking(ISSUES, 1, rows(&[1, 2, 3]), 2),
        Step::linking(ISSUES, 2, rows(&[4, 5]), 3),
        Step::new(ISSUES, 3, rows(&[6])),
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let page = client
        .get_all_pages::<Row, i64, _>(
            &fake.url(ISSUES, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("walk succeeds");

    assert_eq!(numbers(&page.items), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(page.completeness.returned, 6);
    assert_eq!(page.completeness.pages_fetched, 3);
    assert!(
        page.completeness.complete,
        "a fully-consumed Link chain is complete"
    );
    assert_eq!(page.completeness.incomplete_reason, None);
    assert_eq!(fake.requests(), 3, "exactly one request per page");
}

#[test]
fn an_item_straddling_a_page_boundary_appears_once_at_its_first_position() {
    // Page 2 repeats row 2 and page 3 repeats row 4 — an item created between
    // two requests can shift the boundary. Deduplication keeps the first
    // occurrence, so ordering stays server order.
    let fake = Fake::serve(vec![
        Step::linking(ISSUES, 1, rows(&[1, 2]), 2),
        Step::linking(ISSUES, 2, rows(&[2, 3, 4]), 3),
        Step::new(ISSUES, 3, rows(&[4, 5])),
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let page = client
        .get_all_pages::<Row, i64, _>(
            &fake.url(ISSUES, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("walk succeeds");

    assert_eq!(numbers(&page.items), vec![1, 2, 3, 4, 5]);
    assert_eq!(page.completeness.returned, 5);
    assert!(page.completeness.complete);
    assert_eq!(fake.requests(), 3);
}

#[test]
fn a_failing_second_page_never_returns_the_first_page_as_success() {
    // The #1528 contract: page 1 succeeding is not evidence the read is
    // complete. A 500 on page 2 must surface as an error — never as a short,
    // "complete" list.
    let fake = Fake::serve(vec![
        Step::linking(ISSUES, 1, rows(&[1, 2]), 2),
        Step::new(ISSUES, 2, rows(&[3])).failing(500, "Server Error"),
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let result = client.get_all_pages::<Row, i64, _>(
        &fake.url(ISSUES, 1),
        &PaginationPolicy::DEFAULT,
        |r| r.number,
    );

    match result {
        Err(GitHubError::Api(500, _)) => {}
        Err(other) => panic!("expected the page-2 500 to propagate, got {other:?}"),
        Ok(page) => panic!(
            "page 1 came back as success with {} items and complete={} — \
             that is the silent-truncation bug of #1528",
            page.items.len(),
            page.completeness.complete
        ),
    }
    assert_eq!(fake.requests(), 2, "the walk stops at the failure");
}

#[test]
fn a_malformed_later_page_fails_the_whole_read() {
    let fake = Fake::serve(vec![
        Step::linking(ISSUES, 1, rows(&[1]), 2),
        Step {
            body: "not json".to_string(),
            ..Step::new(ISSUES, 2, String::new())
        },
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let result = client.get_all_pages::<Row, i64, _>(
        &fake.url(ISSUES, 1),
        &PaginationPolicy::DEFAULT,
        |r| r.number,
    );
    assert!(
        result.is_err(),
        "a body that cannot be deserialised must fail, not silently yield page 1"
    );
}

/// A repository with 101 open issues. The old single `per_page=100` request
/// dropped issue 101 and the probe could not tell.
#[test]
fn one_hundred_and_one_issues_include_the_hundred_and_first() {
    let first: Vec<i64> = (1..=100).collect();
    let fake = Fake::serve(vec![
        Step::linking(SEARCH, 1, search_envelope(&first, 101), 2),
        Step::new(SEARCH, 2, search_envelope(&[101], 101)),
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let page = client
        .search_all_pages::<Row, i64, _>(
            &fake.url(SEARCH, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("walk succeeds");

    assert_eq!(page.items.len(), 101);
    assert_eq!(
        page.items.last().map(|r| r.number),
        Some(101),
        "issue 101 lives on page 2 and must arrive"
    );
    assert_eq!(page.completeness.reported_total, Some(101));
    assert!(page.completeness.complete);
    assert_eq!(fake.requests(), 2);
}

/// Same shape for the PR-file listing: a PR touching 101 files is not a
/// 100-file PR (issue #1528, requirement 8).
#[test]
fn a_pr_with_more_than_one_hundred_files_reports_every_file_and_completeness() {
    let first: Vec<i64> = (1..=100).collect();
    let fake = Fake::serve(vec![
        Step::linking(FILES, 1, rows(&first), 2),
        Step::new(FILES, 2, rows(&[101])),
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let page = client
        .get_all_pages::<Row, i64, _>(
            &fake.url(FILES, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("walk succeeds");

    assert_eq!(page.items.len(), 101);
    assert_eq!(page.items.last().map(|r| r.number), Some(101));
    assert!(page.completeness.complete);
    assert_eq!(page.completeness.pages_fetched, 2);
}

#[test]
fn a_failing_second_file_page_is_not_a_complete_diff() {
    let first: Vec<i64> = (1..=100).collect();
    let fake = Fake::serve(vec![
        Step::linking(FILES, 1, rows(&first), 2),
        Step::new(FILES, 2, rows(&[101])).failing(502, "Bad Gateway"),
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let result = client.get_all_pages::<Row, i64, _>(
        &fake.url(FILES, 1),
        &PaginationPolicy::DEFAULT,
        |r| r.number,
    );
    assert!(
        matches!(result, Err(GitHubError::Api(502, _))),
        "the second page's failure must propagate — a 100-file list would \
         render as a complete diff"
    );
}

// ---------------------------------------------------------------------------
// Caps, ceilings, and cancellation.
// ---------------------------------------------------------------------------

#[test]
fn a_search_past_the_service_ceiling_is_reported_incomplete() {
    // GitHub serves at most 1,000 matches and then stops advertising a `next`
    // link while still reporting a larger `total_count`. Walking every page
    // therefore does NOT make the result complete — it must say so.
    assert_eq!(SEARCH_RESULT_CEILING, 1000);
    let pages = (SEARCH_RESULT_CEILING as usize) / 100;
    let mut steps: Vec<Step> = (0..pages)
        .map(|p| {
            let start = (p * 100) as i64 + 1;
            let batch: Vec<i64> = (start..start + 100).collect();
            Step::linking(SEARCH, p as u32 + 1, search_envelope(&batch, 2400), p as u32 + 2)
        })
        .collect();
    steps[pages - 1].next_page = None;

    let fake = Fake::serve(steps);
    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let page = client
        .search_all_pages::<Row, i64, _>(
            &fake.url(SEARCH, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("walk succeeds");

    assert_eq!(page.items.len(), SEARCH_RESULT_CEILING as usize);
    assert_eq!(page.completeness.pages_fetched, pages as i64);
    assert!(
        !page.completeness.complete,
        "1,000 of 2,400 matches is not the whole answer"
    );
    assert_eq!(
        page.completeness.incomplete_reason,
        Some(GitHubIncompleteReason::SearchCeiling)
    );
    assert_eq!(page.completeness.reported_total, Some(2400));
    assert_eq!(fake.requests(), pages);
}

#[test]
fn githubs_own_incomplete_results_flag_is_surfaced() {
    let fake = Fake::serve(vec![Step {
        body: serde_json::json!({
            "total_count": 2,
            "incomplete_results": true,
            "items": [{ "number": 1 }]
        })
        .to_string(),
        ..Step::new(SEARCH, 1, String::new())
    }]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let page = client
        .search_all_pages::<Row, i64, _>(
            &fake.url(SEARCH, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("walk succeeds");

    assert_eq!(page.items.len(), 1);
    assert!(
        !page.completeness.complete,
        "GitHub said its own index was incomplete; we must not claim completeness"
    );
    assert_eq!(
        page.completeness.incomplete_reason,
        Some(GitHubIncompleteReason::UpstreamIncomplete)
    );
}

#[test]
fn the_page_budget_stops_the_walk_and_reports_the_cap() {
    // A server that always advertises another page would loop forever without
    // a bound.
    let steps: Vec<Step> = (0..50)
        .map(|p| Step::linking(ISSUES, p + 1, rows(&[p as i64 + 1]), p + 2))
        .collect();
    let fake = Fake::serve(steps);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let policy = PaginationPolicy {
        per_page: 2,
        max_pages: 3,
    };
    let page = client
        .get_all_pages::<Row, i64, _>(&fake.url(ISSUES, 1), &policy, |r| r.number)
        .expect("walk succeeds");

    assert_eq!(page.items.len(), 3);
    assert_eq!(page.completeness.pages_fetched, 3);
    assert!(!page.completeness.complete);
    assert_eq!(
        page.completeness.incomplete_reason,
        Some(GitHubIncompleteReason::SafetyCap)
    );
    assert_eq!(
        fake.requests(),
        3,
        "the budget is checked before the request leaves, so no fourth request"
    );
}

#[test]
fn cancelling_mid_walk_stops_before_the_next_request() {
    // The fake cancels the client while preparing page 1's response, so the
    // flag is provably set before the client can reach the top of the loop —
    // deterministic, not a race on a sleep.
    let steps: Vec<Step> = (1..=3)
        .map(|p| {
            let mut step = Step::linking(ISSUES, p, rows(&[p as i64]), p + 1);
            step.next_page = if p < 3 { Some(p + 1) } else { None };
            step
        })
        .collect();

    let fake = Fake::bind(steps);
    let client = Arc::new(
        GitHubClient::for_test(&fake.base_url, "test-token").expect("test client"),
    );
    let server_side = Arc::clone(&client);
    let fake = fake.spawn(move |index| {
        if index == 0 {
            server_side.cancel();
        }
    });

    let page = client
        .get_all_pages::<Row, i64, _>(
            &fake.url(ISSUES, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("a cancelled read still returns what arrived");

    assert_eq!(
        page.items.len(),
        1,
        "only the page served before cancellation may appear"
    );
    assert!(!page.completeness.complete);
    assert_eq!(
        page.completeness.incomplete_reason,
        Some(GitHubIncompleteReason::Cancelled)
    );
    assert_eq!(
        fake.requests(),
        1,
        "cancellation must stop the walk before request 2"
    );
    assert!(client.is_cancelled());
}

#[test]
fn a_read_cancelled_before_it_starts_issues_no_request_at_all() {
    let fake = Fake::serve(vec![Step::new(ISSUES, 1, rows(&[1]))]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    client.cancel();
    assert!(client.is_cancelled());

    let page = client
        .get_all_pages::<Row, i64, _>(
            &fake.url(ISSUES, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("cancellation is not an error");

    assert!(page.items.is_empty());
    assert_eq!(page.completeness.pages_fetched, 0);
    assert_eq!(
        page.completeness.incomplete_reason,
        Some(GitHubIncompleteReason::Cancelled)
    );
    assert_eq!(fake.requests(), 0, "no API call may be spent");
}

// ---------------------------------------------------------------------------
// Reconciliation ingest must refuse a partial feed (requirement 7).
// ---------------------------------------------------------------------------

#[test]
fn reconciliation_ingest_refuses_a_truncated_trigger_feed() {
    let steps: Vec<Step> = (1..=4)
        .map(|p| Step::linking(ISSUES, p, rows(&[p as i64]), p + 1))
        .collect();
    let fake = Fake::serve(steps);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let policy = PaginationPolicy {
        per_page: 2,
        max_pages: 2,
    };
    let page = client
        .get_all_pages::<Row, i64, _>(&fake.url(ISSUES, 1), &policy, |r| r.number)
        .expect("walk succeeds");

    match super::pagination::require_complete_read(page, "open issues labelled `run`") {
        Err(GitHubError::Incomplete {
            what,
            reason,
            returned,
            ..
        }) => {
            assert_eq!(what, "open issues labelled `run`");
            assert_eq!(reason, GitHubIncompleteReason::SafetyCap);
            assert_eq!(returned, 2);
        }
        Ok(items) => panic!(
            "a truncated trigger feed was accepted as complete with {} items",
            items.len()
        ),
        Err(other) => panic!("expected GitHubError::Incomplete, got {other:?}"),
    }
}

#[test]
fn reconciliation_ingest_accepts_a_complete_feed_unchanged() {
    let fake = Fake::serve(vec![Step::new(ISSUES, 1, rows(&[1, 2]))]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let page = client
        .get_all_pages::<Row, i64, _>(
            &fake.url(ISSUES, 1),
            &PaginationPolicy::DEFAULT,
            |r| r.number,
        )
        .expect("walk succeeds");

    let items = super::pagination::require_complete_read(page, "open issues labelled `run`")
        .expect("a complete feed passes through");
    assert_eq!(numbers(&items), vec![1, 2]);
}

// ---------------------------------------------------------------------------
// Circuit / Autopilot ingest, end to end over the production search path.
// ---------------------------------------------------------------------------

/// The search URL Autopilot actually builds for a labelled issue query.
const LABEL_SEARCH_PATH: &str =
    "/search/issues?q=repo:acme/demo+is:issue+state:open+label:%22run%22&per_page=100";

#[test]
fn label_ingest_consumes_every_matching_page() {
    let first: Vec<i64> = (1..=100).collect();
    let fake = Fake::serve(vec![
        Step {
            next_page: Some(2),
            ..Step::at(LABEL_SEARCH_PATH, issue_envelope(&first, 101))
        }
        .linking_to("/search/issues?page=2"),
        // The second page is served from the `Link` header, so its path is the
        // endpoint's own `?page=2` form.
        Step::new(SEARCH, 2, issue_envelope(&[101], 101)),
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let issues = client
        .list_open_issues_with_label("acme", "demo", "run")
        .expect("complete ingest");

    assert_eq!(issues.len(), 101, "page 2 must not be dropped");
    assert_eq!(issues.last().map(|i| i.number), Some(101));
    assert_eq!(fake.requests(), 2);
}

#[test]
fn label_ingest_fails_visibly_when_a_page_is_lost() {
    let first: Vec<i64> = (1..=100).collect();
    let fake = Fake::serve(vec![
        Step {
            next_page: Some(2),
            ..Step::at(LABEL_SEARCH_PATH, issue_envelope(&first, 101))
        }
        .linking_to("/search/issues?page=2"),
        Step::new(SEARCH, 2, issue_rows(&[101])).failing(503, "Service Unavailable"),
    ]);

    let client = GitHubClient::for_test(&fake.base_url, "test-token").expect("test client");
    let result = client.list_open_issues_with_label("acme", "demo", "run");

    assert!(
        result.is_err(),
        "a trigger feed that lost page 2 must not be handed to the poll pass as \
         a complete list"
    );
    assert!(
        matches!(result, Err(GitHubError::Api(503, _))),
        "the underlying page failure must be preserved for the log"
    );
}

#[test]
fn an_incomplete_read_message_names_what_was_short() {
    let message = GitHubError::Incomplete {
        what: "open issues labelled `run`".to_string(),
        reason: GitHubIncompleteReason::SearchCeiling,
        returned: 1000,
        reported_total: Some(2400),
    }
    .to_string();
    assert!(
        message.contains("open issues labelled `run`"),
        "message must name the read: {message}"
    );
    assert!(
        message.contains("1000") && message.contains("2400"),
        "message must carry the shortfall: {message}"
    );
    assert!(
        message.contains("partial"),
        "message must not read as a complete answer: {message}"
    );
}