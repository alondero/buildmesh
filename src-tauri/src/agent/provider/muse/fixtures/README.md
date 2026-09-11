# Muse MSP telemetry fixtures (issue #1680)

Recorded NDJSON notification sequences for the telemetry ingest seam.
Tests feed these lines into `MuseTelemetryStore` and assert the public
node payload. No live `muse` process is required.

## Redaction policy

Fixtures contain **only** telemetry counters, ids, cursors, and pressure
levels. They must never include:

- prompt or assistant text
- tool names, commands, or arguments
- credentials, tokens, auth headers, or API keys
- workspace paths or source code

`sourceRange` is an opaque provenance token (MSP v1); fixtures use
synthetic numeric placeholders, never record contents.

The `redaction_audit` test walks every `*.jsonl` in this directory and
fails if a forbidden key or secret-shaped value appears.
