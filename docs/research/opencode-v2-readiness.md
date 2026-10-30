# OpenCode 2 integration evidence

**Status:** Implementation evidence for #2655; platform acceptance is incomplete.
**Checked:** 2026-10-30, Windows, `opencode v2.2.3`; V1 executable reports `1.18.3`.

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
`--auto`, so that probe did not execute the exact shipped command. Actual
out-of-project reads or writes remain unverified for both production launch
paths and interactive permission modes.

## Storage and automated evidence

The observed V2 database defines `session_v2`, `session_message` and
`session_part`. Messages use `seq`; native parts use `ordinal` and `call_id`.
Observed assistant content can be inline in `session_message.data.content`,
with user text in `data.text`; the native part table can be empty. The reader
handles both layouts, orders native parts and never falls back to V1 tables.
Idle rows are boundaries, not answers. Reports require the newest non-idle
message to be completed assistant text without tool calls.

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
The ANSI case failed before its parser fix. The old poll-before-read helper
timed out at 1 MiB per stream on this machine; 64 KiB per stream did not stall.

## Remaining acceptance gates

Native Buildmesh TUI spawn/prefill, explicit resume and regenerate, app restart
recovery, real permission-request attention, stale callbacks through the real
attention route, and two concurrent nodes need full app acceptance. The same
checks on macOS and concurrent V1/V2 turns against one database remain required.
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
