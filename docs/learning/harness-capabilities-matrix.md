# Harness capabilities matrix

Human-readable index of the built-in catalog emitted from
`src-tauri/src/agent/harness_catalog.rs`. Values are not hand-authored here —
when an adapter flag changes, regenerate the table with `cargo test` (cwd
`src-tauri/`) and update this matrix so every label still appears as a
`| <label> |` row. `npm run check:docs` enforces that coverage.

The profile id `claude` is not a separate row: it is an alias of **Claude Code**
(`anthropic`).

| Harness | Resume | Attention | Transcript | Model | Effort | Prefill | Extra args | Platforms |
|---|---|---|---|---|---|---|---|---|
| Claude Code | Yes | Hook | Yes | Yes | Closed | Yes | Yes | windows, macos, linux |
| Antigravity | Yes | Hook | Yes | Yes | Closed | Yes | Yes | windows, linux, macos |
| OpenCode | Yes | Hook | Yes | Yes | None | Yes | Yes | windows, linux, macos |
| Codex | Yes | Hook | Yes | Yes | Inline config | Yes | Yes | windows, macos, linux |
| Cursor | Yes | Hook | Yes | Yes | None | Yes | Yes | windows, macos, linux |
| Grok Code | Yes | Hook | Yes | Yes | Closed | Yes | Yes | windows, macos, linux |
| Kimi Code | Yes | None | No | Yes | None | No | Yes | windows, macos, linux |
| MiniMax Code | Yes | None | Yes | No | None | Yes | Yes | windows, macos, linux |
| DeepSeek Harness | Yes | None | No | Yes | None | No | Yes | windows, macos, linux |
| Command Code | Yes | Passive watcher | Yes | Yes | Closed | Yes | Yes | windows, macos, linux |
| Freebuff | Yes | None | No | No | None | Yes | Yes | windows, linux, macos |
| Meta Muse | Yes | Passive watcher | Yes | Yes | None | Yes | Yes | linux, macos |
| Cline | Yes | None | No | Yes | Closed | Yes | Yes | windows, linux, macos |
| Terminal | No | None | No | No | None | No | No | windows, macos, linux |
