# Mobile client

Status: current

## Create PR is node-scoped

A pull request the phone creates comes from an **agent node's worktree**, never from
the mesh root checkout. `POST /api/meshes/{id}/pr` therefore requires `node_id`, and the
route validates at the HTTP boundary that the node belongs to that mesh (403 otherwise)
before doing any work; `commands::pr::create_pr_for_node_source` re-checks the same
invariant because it is also reachable as a Tauri command. A mesh-scoped request can only
resolve `mesh.path`, which published the root's branch: `main -> main` for a root parked
on the base ref, and an unrelated feature branch otherwise — while the sheet displayed the
node's branch.

Both branches come from one resolver, `resolve_pr_source_for_node`:

- `head` is read from `env::node_working_path(&node)` (the Windows-side view — git never
  sees a WSL path), so it tracks the worktree rather than any cached DB field.
- `base` is derived from the mesh's own `base_ref` via `local_base_branch`, which strips a
  remote qualifier (`origin/trunk` → `trunk`) without mangling a branch whose own name
  contains a slash (`feature/x`). The client never has to assume `main`.

`GET /api/meshes/{id}/pr/source?node_id=` returns that same `PrSource` struct, so the pair
the sheet previews is by construction the pair the create path uses — they cannot drift
apart. The create request may carry `head_branch` as an assertion: if the worktree moved
between preview and submit, the backend rejects instead of publishing something the user
never saw. The open-PR recovery from #771 runs *after* source resolution
(`create_pull_request_idempotent` keys it on `head`), so the duplicate check consults the
node's real branch. Omitting `base_branch` lets the mesh decide it.

A node whose worktree branch equals the resolved base is rejected as "nothing to compare"
before GitHub is ever called.

## Sheet dismissal ownership

A sheet with an in-flight request owns its own dismissal. `Sheet` takes a `dismissible`
flag that gates the backdrop; the sheet gates its Cancel button; and it publishes the flag
through `onBusyChange` so the shell's popstate handler honours the same decision. While a
create is in flight the browser/OS Back gesture is *consumed and re-pushed*, so the history
stack keeps its depth and the sheet keeps the work it was protecting.

Completion is scoped to the sheet instance that started it: `prSheet` carries an
`instanceId`, and `onCreated` ignores a completion whose id no longer matches the open
sheet. Without that, a sheet dismissed mid-request (or retired by auth recovery) would
complete into nothing and pop the *screen's* history entry, navigating the user backwards
silently. Submitted fields are frozen while the request is in flight, so the values on
screen always match the ones GitHub received. `Sheet` also carries `role="dialog"`,
`aria-modal`, and focus containment.

## Mobile diff empty states

The diff route diffs a file against the node's **merge base** with its mesh `base_ref`
(ADR 0005), never against HEAD, and the copy says so. A hunk-less diff is not
automatically "no changes": the mobile client consumes the generated `FileDiff` type, so
`binary`, `status`, `old_path` and the `additions`/`deletions` counters are visible to the
screen. It distinguishes a modified binary file, a rename with unchanged contents, a
metadata-only change, and a genuinely unchanged file. Hand-declaring these interfaces
dropped exactly those fields and collapsed all four cases into "No diff (file matches
HEAD)" — a false claim about files that demonstrably changed.

## Mobile task navigation and idea capture

The mobile shell owns screen history and the selected home tab. NodeList owns
Overview/Work polling and attention events; NodeOverview polls the selected
node and sends short replies through the acknowledged HTTP input route.
CaptureIdea retains a browser-local draft until creation succeeds. Its prompt
crosses the generated CreateNodeRequest boundary into SpawnIntent::Prompt,
so the existing spawn orchestrator owns initial prompt delivery. The create
route validates text size and prompt capability before creating a node;
mobile never races terminal startup by injecting the idea as keystrokes.

## Mobile terminal socket lifecycle

The mobile terminal socket is a **separate** fanout (`http::ws`), keyed by node id in one
process-global map, and it treats three lifecycle facts as distinct. **Process exit and
restart** — `kill_session` / PTY EOF / `clear_scrollback` — drop the retained bytes and
**keep** the channel: a restarted agent comes back under the same node id, and the terminal
context a phone reconnects to is intentional. **Permanent node deletion** retires the whole
entry, and that retirement happens only *after* the row delete commits
(`node_teardown::release_after_delete` for the mesh cascade, an explicit
`http::ws::retire_pty_channel` after `delete_agent_node_enqueueing_removal` in
`agent_node::delete`), because retiring records a tombstone that refuses channel creation —
a tombstone applied to a row that survived a failed delete would strand a live node. The
cascade learns which ids to retire from `DELETE ... RETURNING id` rather than a pre-delete
SELECT, so a node created between a snapshot and the delete is not orphaned.

Whether a channel may be created at all is decided by the `agent_nodes` row, which survives
a process restart; the in-memory tombstone set (`RetiredFence`, a `HashSet` for O(1) lookup
plus a `VecDeque` for bounded FIFO eviction) only closes the in-process window where a delete
commits between that check and the create. `handle_ws_connection` therefore refuses an id with
no row before it creates anything, which is why a phone that auto-reconnects to a node deleted
while the app was closed cannot conjure an immortal channel per reconnect. The fence is
consulted under the `KNOWN_NODES` write lock, and `retire_pty_channel` takes both locks in the
same order while holding both, so a create either happens entirely before a delete (and is then
removed) or entirely after it (and sees the tombstone).

Fanout payloads are `Bytes` chunks of at most 32 KiB: one allocation is shared by the ring and
every receiver, and the socket path moves a chunk into the frame without copying. The retained
history is a separate byte `VecDeque` and is *not* one of those sharers — only ring-to-receiver
sharing scales with subscriber count. The ring's slot count is *derived* from a per-node byte
budget rather than picked as a bare slot number, so a subscriber that never drains pins at most
the budget instead of slots times batch size. `send_pty_output` never creates a channel, drops
output for an unknown node, and trims an oversized write before copying it into the history
rather than spiking the buffer to the size of the whole write. A subscriber that lags past the
ring recovers by replaying the history tail. Pins:
`retiring_a_deleted_node_releases_its_channel_and_its_allocations`,
`a_create_racing_a_retire_never_leaves_a_channel_behind`,
`a_node_deleted_before_startup_is_refused_and_creates_no_channel`,
`an_evicted_tombstone_does_not_reopen_the_channel`,
`a_process_exit_keeps_the_channel_so_a_restart_reuses_it`,
`deleting_a_node_closes_its_live_terminal_socket`,
`reconnecting_to_a_deleted_node_closes_instead_of_resurrecting_it`,
`a_reconnecting_client_replays_history_then_receives_live_output`,
`a_slow_subscriber_pins_at_most_the_byte_budget`,
`repeated_node_lifetimes_reclaim_every_entry`.

