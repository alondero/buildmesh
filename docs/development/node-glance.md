# Node Glance

Status: current

A Node Glance answers "what is this agent doing?" without opening its
terminal: the latest tool call, the last message the agent wrote, the prompt it
is working on, and how long it has been in its state and in its session. The
status glyph says *which state* a node is in; the glance says *what it is doing
there*. See [ADR-0054](../adr/0054-node-glance-on-demand.md) for why it is
read on demand rather than pushed.

## Where it appears

| Client | Surface | When it is read |
|---|---|---|
| Android | **Right now** card on the node screen. It leads the screen; for a node waiting on you it follows the request and reply. | Every 5 seconds while the node screen is resumed |
| Browser phone client | **Right now** section on **Work details** | Every 5 seconds while the page is visible and online (`useVisibilityPolling`) |
| Desktop | Hover card beside a sidebar node row, after a 500 ms mouse dwell | Every 5 seconds while the card is open |

Nothing reads a glance on the node-list poll, and the desktop card is not
opened by touch or by keyboard focus moving through the list.

## Wire shape

`GET /api/nodes/{id}/glance` (Admin scope) and the `get_node_glance` Tauri
command return the same `NodeGlance` (`src/types/generated/NodeGlance.ts`),
built by `services::node_glance::read_blocking`.

| Field | Meaning |
|---|---|
| `status` | The node's status when the glance was read |
| `observed_at_ms` | The desktop's clock at that moment |
| `status_since_ms` | When the node entered `status` (`status_changed_at`) |
| `session_started_at_ms` | When the agent process last started; `null` for a node that never ran |
| `created_at_ms` | When the node was created |
| `transcript` | The transcript layer, or `null` |
| `transcript_unavailable` | Why there is no transcript layer, or `null`. Exactly one of the two is present |

The transcript layer (`TranscriptGlance`) carries `last_prompt`,
`last_message`, `message_after_prompt` (the message answers the current
prompt rather than an earlier one), up to three `activity` lines (tool and
target, newest last), `tool_calls_since_prompt`, and `updated_at_ms` (the
store's last write, the harness-agnostic sign of life).

Clients measure elapsed time as `observed_at_ms + (time held) - <timestamp>`,
so a phone never compares its own clock with the desktop's. A missing node is
`404`, a failed row read `503`, and an unreadable transcript a `200` glance that
names the reason. The phone's parser reads the canonical fixtures
`node_glance.json` and `node_glance_unsupported.json`, which
[`http::mobile_wire`](mobile.md#mobile-route-wire-fixtures) derives from the
Rust type.

## How a harness provides it

Every harness with a wired transcript reader serves a glance through two
methods on `TranscriptReader`
(`src-tauri/src/services/transcript_reader/adapter.rs`), both with defaults:

- **`glance_window`** returns the bounded `Parsed` window the glance is shaped
  from. The default reads the last 256 KiB of a JSONL store and keeps the last
  24 turns, falling back to the whole file only when that window holds no
  assistant text (the digest's rule). Stores that are not a JSONL tail override
  it with their own bounded read: Cline's single JSON document and the
  OpenCode 1 and 2 SQLite stores take the same read as a log tail.
- **`describe_tool_call`** turns one `ToolCall` into an activity line. The
  default takes the first of `command`, `cmd`, `file_path`, `filePath`,
  `notebook_path`, `pattern`, `query`, `path`, `absolute_path`, `url`,
  `description` and `prompt` that holds text; a `[shell, -flag, script]`
  command array shows the script, and an `apply_patch` envelope shows the files
  it touches. A harness whose tools carry their subject elsewhere overrides it.

`glance::from_parsed` then shapes the window the same way for every harness:
the latest non-empty user turn is the prompt, the tool calls after it are the
activity, and the parser's whole-window last assistant text is the message.
A new transcript reader therefore gets a glance without further work; the
reader-wide test `every_wired_file_reader_serves_a_glance_from_its_fixture`
runs each reader's fixture through it.

The node-level gate is `coordinator::enrichment::transcript_glance`. It applies
the Node Digest's capability check (`produces_readable_transcript` and a wired
`TranscriptFormat`) without touching the filesystem for an unsupported harness,
and scrubs secrets from every string before the glance leaves the host.

## Cost bounds

- One glance is one row read and, for a supported harness, one bounded
  transcript read. The row read returns its connection before the transcript is
  opened.
- `transcript_reader::read_glance` caches the shaped glance per store, keyed on
  the store's length and modification time plus its SQLite `-wal` when there is
  one. A quiet agent's glance costs a metadata check. Entries expire after 30
  seconds and the cache holds at most 64 stores.
- Only the node a person is looking at is read, at the node list's cadence.

## Limitations

- Harnesses without a transcript reader (Terminal, Kimi Code, Freebuff,
  DeepSeek, Perch) show timings and `unsupported` only. The rendered terminal is
  deliberately not a source, as for the Node Digest.
- Activity is what the harness has written to its transcript. A harness that
  records a turn only when it ends shows the previous turn's activity until
  then, and `Now:` names the latest recorded tool call, which may already have
  finished.
- The default describer can pick a field that is not the most useful one for an
  unfamiliar tool; the tool name is always shown.
