# Fake-GitHub request-counter tests (no new deps)

Learned while pinning issue #1529's O(pages) contract in `src-tauri/src/services/github.rs`.

To assert HTTP cost (not just mapping), spin a raw `std::net::TcpListener`
on `127.0.0.1:0` in the test, count requests with
`Arc<AtomicUsize>`, and serve scripted pages (`(nodes_json, has_next,
cursor)` triples — one `accept()` per expected request). The server thread
`join()`s at the end, so an over-eager client (N+1 regression) blocks the
join and fails loudly instead of passing silently.

Why raw TCP and not `tiny_http`/`wiremock`: the repo avoids extra deps for
trivial work (`percent_encode_path_component` precedent), and the request
body only needs a `Content-Length` drain — headers are otherwise ignored.
`GitHubClient::for_test(base_url, token)` (explicit base URL, no env/token
resolution) points one client at the fake without process-global env races
between parallel tests.

Two shell gotchas from the same session (Windows PowerShell 5.1):
- `cargo test` accepts exactly ONE `TESTNAME` filter positional — pass a
  common substring (`cargo test --lib list_pr_summaries`) instead of several.
- The `grep` alias plus `2>/dev/null` breaks (`Out-File` to `F:\dev\null`);
  use `Select-String` or drop the redirect.
