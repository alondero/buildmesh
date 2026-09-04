# Fake-GitHub request-counter tests (no new deps)

Learned while pinning issue #1529's O(pages) contract in `src-tauri/src/services/github.rs`.

To assert HTTP cost (not just mapping), spin a raw `std::net::TcpListener`
on `127.0.0.1:0` in the test, count requests with
`Arc<AtomicUsize>`, and serve a scripted interaction list in the exact
expected order (GraphQL pages plus, where the fallback is under test, REST
detail responses). Socket lifecycle, stated precisely:
- The guard serves exactly `script.len()` connections, then exits. An
  over-eager client (N+1 regression) gets connection-refused on the extra
  request, so its call returns `Err` and the test fails fast.
- An under-eager client leaves the guard parked in `accept()`; that is
  harmless because every test asserts on payload length / counts BEFORE
  joining, so a short client fails on assertions first and never reaches
  `join`. The parked thread dies with the test process.
- A request of an unexpected kind panics the guard with the request line.

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
