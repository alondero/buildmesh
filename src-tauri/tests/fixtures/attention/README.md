# Attention callback replay corpus

Each JSON file covers one wired hook harness. Cases preserve the field names
used by its wire contract, with sanitized session/workspace identities. The
expected decision, Lifecycle Kind and signal health are literal assertions
against the production normalization entrypoint. Malformed JSON, invalid field
types, non-object bodies and unsupported events are included for every harness.

The initial 39 valid cases passed against the original attention classifier
before extraction. They come from the existing attention-route and transcript
adapter tests and the repository's
[harness contract audit](../../../../docs/learning/harness-attention-reliability.md).
These are representative contract fixtures, not newly recorded live callbacks.
They establish parsing/mapping parity, not installed CLI delivery reliability.

| Harness | Wire contract exercised | Coverage limit |
| --- | --- | --- |
| Claude / Anthropic profiles | Stop, idle Notification, permission, question tool, SessionStart | Compatible model profiles resolve to the Anthropic harness |
| Codex | Stop, PermissionRequest, request_user_input, SessionStart, PostToolUse, Interrupt | Tool results resume only with an outstanding approval marker |
| Cline | agent_end, session_shutdown, unprovisioned tool events | No permission, question or idle file hook is provisioned |
| MiniMax Code | Stop, SessionStart, PermissionRequest, mvs session/workspace | SessionStart delivery remains unvalidated; permission is disabled by launch |
| Kimi Code | Stop, permission/reply, question tool, failure, terminal task Notification | Background task completion also requires the ordering fence |
| Grok | Stop, idle/permission Notifications, question tool, cancellation | Runtime token validation remains in the HTTP gate |
| Antigravity | Settled/background Stop, neutral PreToolUse | No permission/question observer is provisioned |
| Cursor | Stop with and without pending transcript work | No permission/question observer is provisioned |
| OpenCode | Creation, idle/busy, permission, question/reply | Plugin fields are already projected from native events |

Unwired harnesses (including passive watcher integrations) do not acquire a
pushed hook strategy through another harness's payload shape. Isolation tests
exercise unknown harnesses, foreign fields and events, and interleaved malformed
Codex / valid Claude callbacks through ordering and lifecycle publication.
