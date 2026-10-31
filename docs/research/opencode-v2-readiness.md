# OpenCode 2 integration evidence

**Status:** Implementation evidence for #2655; platform acceptance is incomplete.
**Checked:** 2026-10-30 and 2026-10-31, Windows, `opencode v2.2.3`; V1 executable reports `1.18.3`.

The implementation retains V1 and adds `opencode2`. The staged retirement gate
is in [ADR-0050](../adr/0050-opencode-major-version-isolation.md).

## Observed CLI and API contracts

The local full TUI exposes `--standalone`, `--session`, `--prompt` and `--auto`.
It does not expose `--model` or `--fork`. `api --standalone` accepts an HTTP
method, path and JSON `--data` and operates with a private server.

`POST /api/session` accepts a chosen `ses_` id and optional `model` reference.
The CLI wraps the response in `data`. Existing-session model updates require
`POST /api/session/<id>/model` with **`{"model":{"providerID":"...","id":"..."}}`**,
not the bare reference. Success has an empty response body. The model is stored
in `session_v2.model`. An unavailable model produced `provider.no-route`, a
failed idle outcome and the plugin's failure callback.

A missing session returns a nonzero CLI exit, HTTP 404 on stderr and a
`SessionNotFoundError` JSON object on stdout for both lookup and model update.
Creating that exact id then makes `GET /api/session/<id>` return `data.id`.
Ordinary resume now uses this lookup to recover an initial create failure,
with or without a configured model; other errors and missing fork parents
remain errors. Regression fixtures cover those outcomes.

A real standalone turn returned `BUILDMESH_SMOKE_OK`. Forking that session
returned a distinct `ses_` id and a fork boundary referencing the parent's
message. Forking an empty session returns HTTP 400; Buildmesh preserves that
failure rather than appending to the parent.

A turn using the API-selected `openrouter/particle-ai/particle-1` returned
`BUILDMESH_PLUGIN_OK`; the persisted model matched and the idle outcome was
`succeeded`. The installed V2 plugin delivered busy events followed by
`session.idle` to a local HTTP receiver for the configured node and session.
This establishes the private CLI/server/plugin boundary, not full app lifecycle
acceptance or a native TUI input check.

Two V2 `run --standalone` processes then ran concurrently in the same directory,
using distinct assigned sessions and per-node overrides. Both returned their
own `NODE_<id>_OK` marker, and busy/completion callbacks stayed on the respective
node URL with its matching session. No shared project config was needed.

## Configuration and permissions

The V1 CLI rejects a project config containing V2 `plugins` and `permissions`
keys. V2 honors an `OPENCODE_CONFIG` file; `OPENCODE_CONFIG_CONTENT` is not used
by this adapter. Each node therefore receives a separate managed override and
the plugin gets identity through its options, not server environment identity.

A real background turn used the project deny rules embedded by the
adapter, with an additional `--auto`, and was asked to run `echo BUILDMESH_DENY_PROBE`. It
reported that no shell/execute tool was available and returned `DENIED`; no
shell tool call appeared in the JSON stream. The override also contains an
explicit `external_directory` deny. This is a tool availability check under
the current CLI, not proof of OS containment or external-directory enforcement.
Production background argv is `run --standalone --format json` without
`--auto`, so that initial probe did not execute the exact shipped command.
The [Windows follow-up](#windows-native-follow-up-2026-10-31) repeats the
production background posture and tests a real external read through the full
TUI with `--auto`. External writes and other platforms remain unverified.

## Storage and automated evidence

The observed V2 database defines `session_v2`, `session_message` and
`session_part`. Messages use `seq`; native parts use `ordinal` and `call_id`.
Observed assistant content can be inline in `session_message.data.content`,
with user text in `data.text`; the native part table can be empty. The reader
handles both layouts, orders native parts and never falls back to V1 tables.
Idle rows are boundaries, not answers. Reports require the newest non-idle
dialogue message to be completed assistant text without tool calls. TUI
metadata such as `agent-switched` is excluded before applying the row budget;
it is not a chat message or a malformed report.

Fixtures cover fresh/resume/regenerate requests, independent node configs,
version selection, malformed storage, report freshness, archive provenance and
shared V1/V2 cost rollups. Migration copies sharing a session id are counted
once using the larger cumulative cost; known V1 node identities are excluded
from V2 discovery until an explicit migration. The existing JSON-lines decoder and `ses_` validator
are reused with explicit V2 coverage. Plugin fixtures exercise data payloads,
request correlation, session routing, failures and cleanup.

Sandbox-enabled V2 launches are refused before API preparation; background
launches check the same executable major as interactive launches. Deferred
detection cannot overwrite a pin edited while its probe was pending.

Review regressions cover ANSI-coloured version output, concurrent pipe draining
with a 1 MiB payload on each stream, executable-cache invalidation, and V1
interactive/background launch compatibility on unknown or timed-out probes.
Deferred discovery uses that same compatibility rule: an unknown probe result
keeps the V1 profile and withholds V2. The detection regression failed before
the shared-rule fix. A subprocess fixture also checks that the version probe
removes an explicitly supplied `FORCE_COLOR` and sets `NO_COLOR=1` without
changing the parent test process's environment.
The ANSI case failed before its parser fix. The old poll-before-read helper
timed out at 1 MiB per stream on this machine; 64 KiB per stream did not stall.

## Windows native follow-up (2026-10-31)

The native dev app was built from base `107e944580623688882c72a8ecc902560885de70`
plus this issue's follow-up changes, using an isolated worktree profile and
CDP on port 9225. Only the isolated dev instance was launched and stopped. These checks use
Chocolatey's OpenCode 1.18.3 and npm's `@opencode/cli` 2.2.3, with
`openrouter/particle-ai/particle-1` for real turns.

| Boundary | Observation |
|---|---|
| Discovery | The baseline app offered only V1: the first `opencode` on its search path was Chocolatey's executable. The npm package ships an `opencode2` alias pointing at the same V2 executable. Preferring that alias, with the existing shared-command fallback and deferred major gate, made both harnesses appear without editing executable pins. Both new regressions failed before the fix. |
| V1 TUI | A fresh native V1 node completed its prefill, captured `ses_e6c7d2d31ffel68DH6SXuoa7x4`, and reached Ready through a real `session.idle` callback with signal health `ok`. |
| Assigned identity and model | After the transport fix, V2 node 3 persisted `ses_99a88211a5e34e4c9d45c1ad47cdb3ed` with zero messages before the spawn IPC returned. The full TUI then completed its prefill, stored the requested model in `session_v2.model`, and reached Ready with signal health `ok`. |
| Session API transport | The app's first V2 launch returned HTTP 400. The CLI's structured response was `InvalidRequestError`, `Expected a valid JSON body`. Raw native argv accepted the same JSON; Windows PowerShell invocation stripped its quotes. A real `.cmd` fixture regression reproduced the transport failure. The adapter now protects native quotes/backslashes and encodes cmd metacharacters as JSON Unicode escapes. |
| Strict report | The opt-in report test initially rejected the real completed TUI fork as `MalformedRecord`: `agent-switched` metadata had entered the chat parser. Selecting user/assistant/idle rows before budgeting fixed both the deterministic fixture and the live reader. The live test then passed twice on stored version 2.2.3: the completed regenerate fork and the completed external-read-denial turn. |
| Restart recovery | V1's initial attempt ran before deferred discovery published and stayed Suspended; retrying afterward recovered it. Frozen-launch validation now checks a saved native executable through the cached major gate. After the repair, a graceful app restart automatically restored V1 node 1 and V2 nodes 3, 4 and 5 with their original ids. The final startup log contained no new errors or panic-file content. The regression covers valid versions, mismatches, missing paths, foreign runtimes and unchanged menu discovery. |
| Resume and regenerate | Explicit resume kept node 3's assigned id and completed a further PTY-submitted turn. Regenerate returned `ses_e6c688bd6ffeSrwMlvuybTMWr1`; a turn on that fork completed. Comparing the parent's seven message rows before and after the fork turn found identical ids, types and data. |
| Attention | Node 3 reached Ready after each completed turn. A second V2 node, id 4, raised `permission.asked`, became Awaiting Input with signal health `ok`, and displayed the native attention banner. Rejecting through that banner cleared the request and the turn completed. A synthetic stale-session busy callback to the real attention route returned 200 and left node 3's identity and lifecycle unchanged; the app logged that it dropped the callback. |
| Transcript and archive | The real coordinator `/nodes/3/log` and `/nodes` routes returned the native V2 dialogue and latest assistant text. After regenerate, the archive picker discovered the original parent with `provider: opencode2` and `resumable: true`. |
| Native coexistence | V1 node 1 and V2 nodes 3 and 4 ran simultaneously in the same Mesh directory with distinct identities and managed configs. Concurrent PTY submissions to V1 and V2 returned `BUILDMESH_V1_COEXIST_2655_OK` and `BUILDMESH_V2_FORK_2655_OK`. Each node reached Ready under its own session id. |
| Production background argv | `run --standalone --format json --model openrouter/particle-ai/particle-1`, with the adapter's exact managed background config and a stdin prompt, exited 0. It exposed no shell or read tool, and did not execute the requested command or file read. This establishes tool removal, not a tool-denial event or OS containment. |
| External-directory rule | Both a direct `run --auto` probe and native V2 node 5's full TUI under the unattended default attempted `read` on `C:/Windows/win.ini`. The native tool recorded `permission.rejected`, `executed: false`, and `Permission denied: external_directory`; the assistant returned `BUILDMESH_NATIVE_EXTERNAL_DENIED_2655` and reached Ready. A sibling fixture inside the containing Git worktree remained readable: V2 treats that worktree as internal even when the active directory is a subdirectory. |
| Concurrent CLI turns | V1 and V2 were launched concurrently in one fixture directory with the V1 plugin present and a separate V2 config. Both wrote their own expected markers to distinct sessions in the shared database. V2 exited 0; V1 completed its response but did not exit before the 60-second deadline and its process tree was terminated. This is qualified CLI evidence, not a passing native coexistence check. |

The live strict-report test is
`services::transcript_reader::readers::opencode2::tests::live_completed_report_snapshot`.
It is ignored during the regular suite. To select it, set
`BUILDMESH_OPENCODE2_REPORT_DB` to the native store path,
`BUILDMESH_OPENCODE2_REPORT_SESSION` to a completed session id, and
`BUILDMESH_OPENCODE2_REPORT_TEXT` to its expected final answer, then run that
exact test with `--ignored --exact --nocapture`. The Windows check used
`compileOwnedTestBinaries` and `run` from `scripts/rust-test-shards.mjs` so it
executed this worktree's binary. The test opens the store read-only and prints
the stored CLI version beside the report assertion.

## Remaining acceptance gates

The same native acceptance checks on macOS remain required.
Actual external writes and OS containment are not established by the read
and tool-availability probes above. V1 default changes and removal still require
the acceptance gate and an explicit stored-row migration in ADR-0050.
Question and native child/subagent event contracts are unverified. Circuit
foreground wiring is outside this issue's adapter scope.

A three-node coexistence probe stopped before starting its turns: V1's
`debug config` with the V1 attention plugin did not exit within 75 seconds. Its
process tree was terminated. This is an unresolved runtime observation, not a
baseline attribution or evidence that concurrent V1/V2 execution passed.

## Primary references

- [V2 plugin configuration](https://opencode.ai/v2/docs/plugins)
- [V2 server plugin lifecycle and event subscription](https://opencode.ai/v2/docs/build/plugins/server/)
- [V2 configuration](https://opencode.ai/v2/docs/config)
- [V2 permissions and external directories](https://opencode.ai/v2/docs/permissions/)
