# ADR-0054: The Node Glance is read on demand from the transcript readers

Status: accepted

The status glyph tells a person which state an Agent Node is in, but not what
it is doing there, so they open its terminal to find out. On the phone the
terminal is screens deep and hard to read. The Node Glance gives that answer
instead: the agent's latest tool call, its last message, the prompt it is on,
and how long it has been at it.

## Decision

1. **One harness seam, with defaults.** The glance is shaped from the bounded
   `Parsed` window every transcript reader already produces. Two
   `TranscriptReader` methods own the harness-specific parts:
   `glance_window` (which bounded read) and `describe_tool_call` (how a tool
   call is named). Both have defaults that fit every wired JSONL reader, so a
   harness adopts the glance by having a transcript reader at all.
2. **Read on demand, never pushed.** A glance is computed only when a client
   asks for one node, through `GET /api/nodes/{id}/glance` or
   `get_node_glance`, and the client asks only for the node a person is looking
   at. The node-list poll and the lifecycle events are unchanged.
3. **Cached on the store's change stamp.** The shaped glance is cached against
   the transcript's length and modification time (and SQLite `-wal`), so
   re-reading a quiet agent costs a metadata check.
4. **Degrade and flag, as the Node Digest does.** A harness without a readable
   transcript, or a transcript that cannot be read, still returns the node's
   timings with an explicit reason. The rendered terminal is not a source
   ([ADR-0008](0008-coordinator-control-api.md) §3).

## Alternatives considered

- **Push the glance with lifecycle events.** Lifecycle callbacks arrive at turn
  boundaries, so a pushed glance would be stale for the whole of a long turn,
  exactly when a person wants it. Pushing on every transcript write would mean
  a watcher per node and an event per tool call to every client, for a view that
  is open for one node at a time.
- **Add the glance to the node list.** The phone re-reads the list every five
  seconds while it is open. Reading every node's transcript on that poll would
  turn a row read into N file reads to show one node's summary.
- **Derive it from terminal output.** The rendered TUI differs per harness and
  version and carries no structure to separate a tool call from prose; the
  readers already parse the structured store.
- **Ask a model to summarise.** It would add latency, cost and a dependency on
  a model being configured, for facts the transcript states directly.

## Consequences

- Every harness with a transcript reader gets a glance; one without shows
  timings and `unsupported`. Adding a reader adds a glance, and the reader-wide
  fixture test checks it.
- Activity is as fresh as the harness's transcript writes. A harness that writes
  only at turn end shows the previous turn's activity until it does.
- See [Node Glance](../development/node-glance.md) for the wire shape, the
  clients and the cost bounds.
