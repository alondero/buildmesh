# Fuzz corpora

Raw byte streams fed to the in-tree fuzz targets. One directory per target;
the harness that reads it lives beside the parser it exercises.

- `http_request/` — raw HTTP request bytes for `http::fuzz`, the embedded
  server's request-read path (`http::server::read_request_head` →
  `http::router::content_length` → `http::request::read_body_with_cap`).

Run it from `src-tauri/`:

```text
cargo test --lib http::fuzz                                          # smoke
BUILDMESH_FUZZ_CASES=500000 cargo test --release --lib http::fuzz -- --nocapture
```

Adding a seed means adding its expected outcome to the harness's pinned table;
an unpinned corpus file fails the run, so a seed can never sit in the directory
without someone saying what it is for. The invariants, the knobs, and why this
is not a `cargo-fuzz` target are in
[`docs/development/remote-access.md`](../../docs/development/remote-access.md#fuzzing-the-request-read-path).