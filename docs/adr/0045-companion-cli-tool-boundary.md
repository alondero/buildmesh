# 45. Companion CLI tool boundary

Status: accepted

The [Companion PRD](../specs/prd-companion-ai.md) needs subscription-backed
inference while Buildmesh owns tool authorization. Companion uses the shared
background CLI launcher with a structured JSON stdio loop. Native CLI tools,
custom settings and MCP servers are disabled, and a separate authentication
probe must report a first-party subscription login before inference starts.
The first supported adapter is host-native Claude. Provider routes and extra
CLI arguments are rejected rather than allowing them to replace this boundary.

The alternative of granting an unrestricted agent the coordinator HTTP token
would let it bypass task-specific spawn caps and confirmation gates. Companion
instead dispatches a closed tool enum over existing coordinator/spawn services;
model output cannot approve its own destructive actions. A task owns one spawn
budget across all calls and one revision-bound, single-use confirmation.

The Rust task outlives its Omnibar sheet. Cancellation kills the inference
process tree, rejects a pending confirmation, and stops subsequent actions.
An already-started provisioning operation settles and reports its result before
the task ends; cancellation does not roll back completed worktrees. Event
snapshots carry task generation and revision so delayed responses cannot replace
newer frontend state. Task history is in-memory and does not survive app exit.

Reviewer reconfiguration changes frozen launch snapshots transactionally and
advances Circuit history to fence stale worker decisions. A reviewer whose step
already exists is rejected: its process or a queued spawn effect may already
own that launch. Replacing an active reviewer requires a fresh review rather
than changing only its advertised provider.

Tests cover spawn-budget accumulation, task ownership, confirmation identity,
snapshot ordering, and rollback of reviewer batches. Live CLI and desktop
acceptance remain separate from these unit checks. See the
[user workflow](../user-guide.md#coordinate-agents-with-companion) for setup,
supported configurations, and recovery.
