# Freebuff SDK surface

Audience: Buildmesh contributors wiring Freebuff. Status: current as of
2026-10-28. This records what the public SDK actually exposes, and what an
installed Freebuff CLI does not. It is not a user-facing promise that the
interactive CLI streams lifecycle events.

## What you can rely on

Buildmesh's interactive Freebuff adapter still launches the CLI in a terminal.
That process does not emit the SDK's print-mode stream. A separate controlled
run, `agent::freebuff_sdk`, stores a successful SDK `RunState` and folds
print-mode events into Circuit lifecycle facts and an assistant report.

Child agents named by `subagent_start` and `subagent_finish` are tracked.
A missing child id or an unknown event type makes child work explicitly
unavailable. An `error` event does not settle the run by itself. The sourced
stream errors (`stream-interrupted`, `output-limit`, `provider-error`) are
retries. An error with no `source` is a tool message the SDK continues past,
so a later report, child registry, and `finish` still count. If that error is
the last event and the run never finishes, child work is unavailable and the
fold reports a failure.

## Installed CLI, checked 2026-10-28

The global `freebuff` command on this machine was npm package launcher
`0.0.162` running native binary `0.2.34` (`freebuff --version`). Its package
ships `index.js`, `launcher.js`, `http.js`, and `README.md`, and depends on
`tar`. It does not depend on `@codebuff/sdk`. `freebuff --help` offers `login`
and `--continue [conversation-id]` only. There is no print, JSON, or SDK mode.

The CLI login remains `~/.config/manicode/credentials.json` (`default.authToken`).
That file is the Usage Meter credential. It is not an SDK API key.

## Public SDK, read from source the same day

Repository: [CodebuffAI/freebuff](https://github.com/CodebuffAI/freebuff).
Package `@codebuff/sdk` version `0.10.7` (`sdk/package.json`).

- [`sdk/README.md`](https://github.com/CodebuffAI/freebuff/blob/main/sdk/README.md)
  shows `CodebuffClient.run({ previousRun, handleEvent })`. `previousRun` is
  the JSON `RunState` from the previous successful call.
- [`sdk/src/client.ts`](https://github.com/CodebuffAI/freebuff/blob/main/sdk/src/client.ts)
  requires `apiKey`, `CODEBUFF_API_KEY`, or `byok`. It does not call
  `getUserCredentials`.
- [`sdk/src/run-state.ts`](https://github.com/CodebuffAI/freebuff/blob/main/sdk/src/run-state.ts)
  defines `RunState` as `sessionState`, `output`, `traceSessionId`, and an
  optional non-secret `inference` pin.
- [`common/src/types/print-mode.ts`](https://github.com/CodebuffAI/freebuff/blob/main/common/src/types/print-mode.ts)
  is the event union: `start`, `text`, `tool_call`, `tool_result`,
  `subagent_start`, `subagent_finish`, `finish`, `error`, `reasoning_delta`,
  `download`.
- [`docs/agents-and-tools.md`](https://github.com/CodebuffAI/freebuff/blob/main/docs/agents-and-tools.md)
  describes subagents as executable `handleSteps` code. Buildmesh records
  their ids. It does not import or run those agent modules.

No live `@codebuff/sdk` call was made. The mapper tests feed the documented
JSON shapes.

## What Buildmesh stores

`DirectoryRunStateStore::for_app_data` writes
`<app data>/freebuff-sdk-runs/<key>.run.json`. The key is a single path
segment. The file is the JSON object to pass back as `previousRun`. An output
whose `type` is `error`, or a value that is not an object with an `output`
object, is refused and leaves the previous file in place.

The interactive adapter stays unwired. A terminal session must not be read as
this stream.

## Out of scope

Freebuff Desktop workspace management and the hosted model catalog stay out
of Buildmesh. This note does not add a Node dependency or shell out to the SDK.
