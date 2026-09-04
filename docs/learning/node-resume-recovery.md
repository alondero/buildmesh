# Node recovery after an app restart

Issue #1555 exposed a gap between durable Agent Nodes and harness-owned
conversation identities. Shutdown correctly suspended the nodes, but startup's
SQL filter hid NULL identities and a one-time Codex migration never retried them.

Startup now lists every Suspended node. Recovery runs concurrently for the
independent nodes, then the normal Resume intents run. Each provider adapter
owns discovery of its Codex rollout metadata, OpenCode SQLite sessions,
Command Code session headers, or Antigravity brain transcripts; the shared
service only applies the workspace/time and ambiguity rules. It does not start
capture pollers: they require a live process and use a fresh clock. An
unsuccessful lookup is retried at the next startup, regardless of the old
`codex_legacy_session_backfill_v1` flag.

Recovery requires a matching Node Working Directory and one distinct session ID
created between two seconds before and five minutes after the launch anchor.
The five-minute allowance covers delayed harness initialization (a live OpenCode
example flushed after 133 seconds). Codex child threads are excluded. AGY must
have a workspace anchor; JSON-quoted Windows paths are decoded before matching.
OpenCode's historic query constrains time without a global newest-50 limit.

Fresh intent atomically clears the old identity and records
`agent_nodes.session_started_at` in epoch milliseconds. Resume leaves both
intact. Legacy nodes without this timestamp use node creation time. Recovery
rejects legacy nodes with distinct later conversations, since a regeneration
may have replaced the original conversation without leaving a durable timestamp.
It conditionally writes only if the node is still Suspended, its provider/worktree
and launch timestamp are unchanged, its identity is missing, and no other node
already owns that ID. The spawn claim precedes identity clearing so duplicate
launches cannot erase the winning request's identity.

Missing or ambiguous transcripts remain Suspended with a **Missing session ID**
badge and the existing Regenerate action. Its message tells the user to use
Regenerate to start a new conversation; it does not imply that another restart
will retry an already completed startup. No fresh conversation is silently
substituted. Suspended Autopilot nodes without an ID are excluded from historic
recovery and this badge because they may be awaiting sandbox approval.

Limits: legacy regenerated nodes have no durable last-launch timestamp; records
outside the bounded window, unknown AGY workspaces, missing files, and ambiguous
matches need manual recovery. The badge describes a missing saved identity, not
proof that the underlying transcript has been deleted. Recovery does not verify
provider authentication or guarantee that a CLI accepts an otherwise valid ID.

Regression tests cover actual provider file/DB formats, delayed flushes, child
threads, cross-project rejection, the old migration flag, empty IDs, duplicate
ownership, generation changes during recovery, and atomic fresh-start writes.
