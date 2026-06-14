# Coordinator Read API — User Guide

> **Audience:** anyone wiring an external agent (Hermes, an in-app superagent, a
> cron, a script) to Buildmesh's read surface. **Prerequisite:** Buildmesh
> running on the same machine, local or remote-reachable over your own tunnel.
>
> **Architecture & rationale:** [`docs/adr/0008-coordinator-control-api.md`](../adr/0008-coordinator-control-api.md).
> **Domain language:** *Coordinator*, *Node Digest* in [`CONTEXT.md`](../../CONTEXT.md).
> **Spec:** [issue #312](https://github.com/alondero/buildmesh/issues/312).

## What this is

Buildmesh exposes a small, read-only HTTP surface through which an external
**Coordinator** can scan every [Agent Node](../../CONTEXT.md) across every Mesh
and drill into any one. The surface is **deliberately agent-agnostic** — Hermes
is the first client, a future in-app Buildmesh superagent the second. The wire
shape is plain JSON over HTTP, designed as MCP *resources* so a future native
MCP wrap is mechanical.

The whole surface is one question: *"across all my nodes, which one needs me
right now, and what is it actually asking?"* Everything in this doc is in
service of that question.

## Quickstart (60 seconds)

1. Open **Buildmesh → App Settings → Coordinator Read API**.
2. Tick **Enable coordinator read API**.
3. Click **Generate token**. The token is **shown once** — copy it then.
   Regenerating invalidates the old one.
4. From any process on the same machine, the embedded HTTP server is already
   listening on the loopback port. Default is **1992** for the stable profile,
   **2992** for the dev profile.

```bash
TOKEN="<paste-the-token-you-just-copied>"
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:1992/nodes | jq '.[0]'
```

If you get a 401 you have the wrong token (or haven't enabled the API).
If you get a 200, you are driving the Coordinator surface.

## The two endpoints

Both endpoints are authenticated with a single bearer token, scoped `read` only.
The mobile root token is **not** accepted on these routes — they are a
deliberately separate capability.

### `GET /nodes` — cheap scan, every node across every Mesh

Returns a JSON array of **Node Digests**, layered: an always-available spine
plus a transcript-derived rich layer for the Claude Code family only. Designed
to be polled on the order of seconds.

```bash
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:1992/nodes
```

Response shape (one element, abridged):

```json
{
  "id": 42,
  "name": "fix-login",
  "mesh": "core",
  "provider": "anthropic",
  "status": "awaiting_input",
  "needs_feedback": true,
  "waiting_since": "2026-06-14T10:00:00Z",
  "last_activity": "2026-06-14T10:00:00Z",
  "enrichment": {
    "status": "available",
    "last_assistant_message": "Should I add the OAuth flow, or just stub it for the demo?"
  }
}
```

A digest whose provider has no readable transcript (OpenCode, Codex, etc.):

```json
{
  "id": 43,
  "name": "docs-pass",
  "mesh": "core",
  "provider": "codex",
  "status": "running",
  "needs_feedback": false,
  "waiting_since": null,
  "last_activity": "2026-06-14T09:58:00Z",
  "enrichment": {
    "status": "unavailable",
    "reason": "unsupported"
  }
}
```

A digest whose provider *does* have a transcript but the file is missing
(no session captured yet, or the file is mid-write):

```json
{
  "id": 44,
  "name": "spike",
  "mesh": "core",
  "provider": "anthropic",
  "status": "running",
  "needs_feedback": false,
  "waiting_since": null,
  "last_activity": "2026-06-14T09:55:00Z",
  "enrichment": {
    "status": "unavailable",
    "reason": "no_session"
  }
}
```

The `enrichment` field is **always present and always tagged** —
degrade-and-flag, never a silent omission (ADR-0008 §3). A Coordinator can
therefore always tell *"the node is quiet"* from *"the rich layer is down"*.

### `GET /nodes/{id}/log?tail=N` — drill into one node

Returns the last `N` raw transcript turns for one node (assistant text + tool
calls). Use this when the digest isn't enough — when you need to understand
*what* the node has been doing over its last several messages, not just the
most recent one.

```bash
curl -sS -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:1992/nodes/42/log?tail=10"
```

- `tail` defaults to 10; the route is bounded so a 50 MB transcript cannot
  stream out the HTTP socket.
- An unknown node id is a **404**. Every other degrade path (unsupported
  provider, no session, file missing/unreadable, JSONL shape change) is a
  **200** carrying the same `{"status":"unavailable",...}` envelope as
  `GET /nodes` — so the consumer always gets a structured answer.
- Content is **raw, not pre-summarised**. The Coordinator is itself an LLM;
  handing it the real material is cheaper and less lossy than spending
  Buildmesh's tokens on a summary. Buildmesh's own LLM is reserved for the
  human UI.

Response shape:

```json
{
  "status": "available",
  "turns": [
    {
      "role": "user",
      "text": "Please fix the login flow",
      "tool_calls": []
    },
    {
      "role": "assistant",
      "text": "Should I add the OAuth flow, or just stub it for the demo?",
      "tool_calls": []
    }
  ],
  "last_assistant_message": "Should I add the OAuth flow, or just stub it for the demo?"
}
```

## Field reference

### Spine (always present)

| Field | Type | Meaning |
|---|---|---|
| `id` | integer | Buildmesh's node id. Use this to drill in. |
| `name` | string | User-visible node name (the same one shown in the sidebar). |
| `mesh` | string | Owning Mesh's name. A Coordinator can group on this. |
| `provider` | string | Provider id (`anthropic`, `minimax`, `kimi`, `codex`, `agy`, `opencode`, `terminal`). |
| `status` | string | Lifecycle status. Most often `running`, `awaiting_input`, `idle`, `pending`, `error`, `suspended`. |
| `needs_feedback` | bool | `true` **iff** `status == "awaiting_input"`. The single highest-value scan field — answers *"which nodes need me right now?"* without the consumer interpreting status strings. |
| `waiting_since` | RFC3339 timestamp or `null` | When the node entered `awaiting_input`. Non-null only while it is waiting. Sort by this **descending** to find the node that has been stuck longest. |
| `last_activity` | RFC3339 timestamp | The node's last lifecycle transition. The spine's "working vs gone quiet" signal. |
| `enrichment` | object | The transcript-derived rich layer. Always present; see below. |

### Enrichment (always present, sometimes `unavailable`)

When `enrichment.status == "available"`, the only extra field is
`last_assistant_message` (a truncated snippet of the agent's most recent
assistant text). When `enrichment.status == "unavailable"`, the only extra
field is `reason`, which is one of:

| Reason | Meaning | Coordinator action |
|---|---|---|
| `unsupported` | The provider does not produce a readable transcript (OpenCode, Codex, Agy, Terminal). | Use the spine only. This is permanent. |
| `no_session` | The provider supports transcripts but Buildmesh has not captured a CLI session id for this node yet (e.g. just spawned). | Retry on the next poll. |
| `no_transcript` | A session id exists but no JSONL file was found on disk where Buildmesh looked. | Retry on the next poll. |
| `unreadable` | The file exists but the I/O failed. | Retry, then page if persistent. |
| `shape_changed` | The file was read but the Claude Code JSONL shape has changed (renamed/missing fields). | **Page** — this is a real format break, not a quiet node. |
| `no_turns_yet` | The file parsed cleanly with no malformed lines, but it also contains no recognizable turns. Almost always a cold-start cwrap node whose JSONL has only `summary`/`mode`/`system` lines so far. | Retry on the next poll. **Not** a format break. |

### Transcript tail (`/nodes/{id}/log`)

When the response is `available`, it carries `turns: [{role, text, tool_calls}]`
and `last_assistant_message`. `role` is `"user"` or `"assistant"`. `tool_calls`
is an array of `{name, input}` — `input` is the raw tool input object with each
string leaf truncated to bound payload size, but the **shape is preserved** so
the Coordinator can read the real structure of every tool call.

When a node is `awaiting_input`, its `last_assistant_message` *is the question
it is blocked on* (ADR-0008 §4). This is the single highest-value field in the
whole API.

## Mental model for a consumer (Hermes)

The four queries that cover 95% of a Coordinator's day:

1. **"Which node needs me right now?"**
   ```bash
   curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:1992/nodes \
     | jq '[.[] | select(.needs_feedback)] | sort_by(.waiting_since) | reverse'
   ```
   This is the whole triage loop. `needs_feedback == true` already filters to
   `awaiting_input`; sorting by `waiting_since` puts the longest-stuck node
   first.

2. **"What is that node actually asking?"**
   Read `last_assistant_message` from the same digest. No second call.

3. **"What has it been doing for the last few minutes?"**
   ```bash
   curl -sS -H "Authorization: Bearer $TOKEN" \
     "http://127.0.0.1:1992/nodes/$ID/log?tail=20"
   ```
   Only when the digest isn't enough.

4. **"Is the rich layer down or is the node just quiet?"**
   Check `enrichment.status`. `unavailable` is *not* an error — it is a
   deliberate signal meaning *"spine is reliable, rich layer can't speak right
   now"*. The `reason` tells you whether to retry, ignore, or page.

## Security posture

Buildmesh is **secure-by-default-off**, loopback-bound, behind a separate
read-scoped token. The defaults are chosen so a naive install (a colleague
cloning the repo, a friend grabbing the binary) is safe by construction:

- The API ships **off**. You must explicitly enable it and mint a token.
- The HTTP server binds **loopback + LAN only**. Reaching it from outside your
  machine is your deliberate choice — run your own tunnel (Tailscale,
  Cloudflare Tunnel, WireGuard, SSH). Buildmesh does not open an internet port.
- The coordinator token is **distinct from the mobile root token**. Granting
  read access does not hand over the mobile session.
- The token is **capability-scoped** (`read` only here). Even if leaked, it
  cannot drive nodes — driving is a separate PRD (#313) with a separate
  `drive` scope.
- The Settings UI shows a **"shown once, copy it now"** banner when the
  token is minted, because we have no place to re-display it. The master
  switch disables the API outright.

The threat model assumed by this surface is *"a coordinating agent on a
machine I control, reached over my own tunnel."* It is not designed for
*"an autonomous agent on a public VPS, reachable from the open internet"*
— that threat requires the *user's* tunnel, not Buildmesh's.

## What this deliberately does NOT do

Out of scope for this PRD, deferred by design (see ADR-0008):

- **Driving nodes.** Sending prompts is the **drive-side PRD** (#313), which
  depends on the `#178` `AgentDriver`. A read token cannot drive a node
  even if the drive surface later lands.
- **A native MCP server.** Plain JSON-over-HTTP is the wire format *now*;
  a thin MCP server is the fast-follow once the read model is proven.
- **Parsing the rendered terminal/TUI.** The transcript is the read source;
  the terminal is for humans.
- **Non-Claude-Code transcript enrichment.** OpenCode, Codex, Agy, and
  Terminal all degrade to a spine-only digest flagged `unsupported`. A
  bespoke reader for any of them is future work — the capability flag
  exists to make that a one-place change.
- **Pre-summarising transcripts** for the Coordinator. Summarisation stays
  in the human UI.
- **Buildmesh opening its own internet-facing port / TLS.** The user owns
  the tunnel.

## Known follow-ups (open issues)

These do not block adoption, but a Coordinator may want to know about them:

- **#339** — A fresh cwrap node's first-seconds digest can read as
  `shape_changed` for a brief window before the first assistant turn lands.
  The fix (a distinct `no_turns_yet` reason, already implemented) is
  queued behind a real-format-break test.
- **#335** — Five-item follow-up from the R2 code review: empty-vs-shape-changed
  conflation (the headline is #339), large-file streaming, tool-call cap,
  multi-block discovery test, live-`curl` verification.

## Files for the implementer

- `src-tauri/src/coordinator/node_digest.rs` — the pure Node Digest builder
- `src-tauri/src/coordinator/enrichment.rs` — provider-capability gate + path
  resolution + bounded transcript read
- `src-tauri/src/services/transcript_reader.rs` — the JSONL parser (quarantines
  all Claude-Code-format brittleness)
- `src-tauri/src/http/routes/coordinator.rs` — thin route handlers
- `src-tauri/src/http/mod.rs` — dispatcher with the off-by-default auth gate
- `src/components/AppSettings/AppSettingsModal.tsx` — the Settings UI section
