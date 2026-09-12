# Circuit run 99: unreadable Grok completion

Investigated against `6102e5ca52d029fe9329e66be0f7787e67c9007f` on Windows.
Read-only inspection of the stable profile ledger and Grok session files:

- Run 99 borrowed Grok agent 3905 and waited at `await_source`.
- Grok recorded `turn_ended`, outcome `completed`, at 2026-09-12 08:00:28.272 UTC.
  The first turn ended with an assistant report describing the implementation
  and its test results.
- At 08:01:28.809 UTC, Buildmesh's lost-turn watchdog observed 60 seconds of
  quiet and synthesized a turn notification, moving the node to awaiting input.
- The gate failed at 08:16:36 UTC without ever recording a report revision.
- The persisted working directory contains mixed Windows separators. Grok's
  session directory percent-encodes its all-backslash working directory; the
  locator encoded the persisted spelling literally and missed the file.
- The actual chat history uses `type: assistant/user/tool_result/reasoning`,
  whereas the adapter and assistant-record freshness predicate required `role`.
  Native tool calls also encode JSON in `arguments`, whereas the adapter read
  only `args`.

The 15-minute timeout was a downstream symptom of missing report evidence.
The 52-minute total includes approximately 37 minutes of legitimate agent work.
The watchdog recovered the missing lifecycle notification, but could not repair
transcript lookup or parsing. The agent subsequently received another user turn;
replaying the old circuit automatically would therefore review changed state.

The fix keeps Grok-specific compatibility inside its transcript adapter. Exact
path lookup remains first, followed by Windows separator normalization. Parsing
and assistant-record detection share the native/legacy record discriminator.
Native tool-call arguments are decoded through the existing bounded tool-input
handling.

Regression tests exercise the file locator plus real circuit report reader,
preserve assistant revision identity across user/tool records, recognize a
repeated assistant response as a new revision, and retain legacy role records.
No running agent or production ledger was modified.

## Verification

- Before the fix, `grok_native_transcript_recovers_circuit_report` failed:
  the completed native assistant report was unreadable (one executed test).
- `scripts/check.ps1 rust -SerialRust`: 3,184 library tests and 18 integration
  tests passed; 20 library tests and one doctest ignored. Mobile assets were
  built first with `npm run build:mobile`.
- `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets`
  completed successfully with warnings outside the changed lines; no baseline
  Clippy comparison was run.
- Final `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib grok
  -- --test-threads=1`: all 60 tests passed, including the combined mixed-path
  report recovery test.
- `npm run check:agent -- --base
  6102e5ca52d029fe9329e66be0f7787e67c9007f` and `git diff --check` passed.
- Standards and spec reviews found no blocking issues.

These checks exercise production parsing and file lookup using synthetic records
matching the captured native schema. They do not establish a live circuit rerun
in the stable app; the source fix requires rebuilding/restarting the app.
