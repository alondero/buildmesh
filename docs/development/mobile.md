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

### PR-source failures are typed, not string-matched

`resolve_pr_source_for_node` returns `PrSourceError`, and each variant maps to exactly one
status: `NotFound` → 404, `NotOwned` → 403, `SameBranch` / `BranchUnknown` / `StaleHead` →
422, `Other` → 500. The routes read `err.status()`; **they never match on the message
text**. An earlier version chose 403 with `starts_with("Agent node")`, and three distinct
failures share that prefix — so an ordinary validation failure came back as 403. Because
`isAuthError` treats 403 as an expired session, the phone answered by clearing the token
and bouncing the user to the pairing screen (issue #2190 review).

This is why the status is part of the contract rather than an implementation detail:
**only a genuine authorization failure may reach `onAuthFailed`.** A 422 stays in the
sheet's error slot, where the user can pick another node. The same reasoning applies to
any new mobile route that resolves user-supplied identity — a 4xx that is not 401 must
not be able to log someone out.

## PR status and squash merge

Open a node's **Work details** to see its open pull request, combined check
state, review decision and mergeability. The browser and native Android client
read the live worktree branch from the git branch route and match it to the
same-repository PR's `head_ref`; the node's stored base reference is not the
source branch. Fork PRs with a coincidentally matching branch are excluded.
The mesh's **Pull requests** shortcut lists open PRs without merge controls for
unrelated work. An incomplete feed states its returned count, reported total
when available, and reason; absence from a truncated feed does not establish
that a node has no PR.

The clients poll while foregrounded and offer **Refresh PR status**. Unknown
mergeability, drafts, conflicts, policy blocks, behind-base branches, failing
or pending checks, and required or changes-requested reviews disable merge.
Missing check/review fields are shown as not reported, rather than passing or
approved. Checks and reviews are nullable generated fields on
`GitHubPullRequest`, fetched with mergeability in the existing GraphQL summary
query, without per-row enrichment requests.

**Squash and merge** asks for confirmation with the PR number and branch
deletion. Confirm rechecks the mergeability route, then submits the PR URL to
the existing merge route (squash plus delete branch). GitHub remains the
authority for permissions and branch protection. An acknowledged merge removes
the PR and reports success. A failed request shows an error; refresh its status
before retrying, especially after an interrupted connection where the merge
may already have happened. Browser responses are versioned against refresh,
mutation and unmount; native polling is owned by the screen and foreground
lifecycle, and is cancelled while merging.

## Circuit Runs on the phone

A node that is inside a Circuit Run shows it. The browser client puts one line on
the node card, for example `Needs you: Review Blueprint · Round 2 of 3 · Approval gate:
Needs approval`, and a **Circuit Run** panel on the node screen with the latest
reviewer verdict and report. The line turns amber only while the run is waiting for a
person. Wording comes from the desktop's own `circuitVocabulary` and `runNextSteps`
modules, so a state reads the same on both. The native Android client does not show
Circuit Runs yet.

`GET /api/circuit-runs` is the single read: one `NodeCircuitRun` per *implementation*
node (the run's source agent, or an agent a circuit step owns) whose run is queued,
running or paused, or whose failed run nobody has acknowledged. "Dismiss all" on the
desktop clears the same failed runs from the phone. A reviewer helper agent is part of
its source's run and is not listed on its own. The clients refresh it with the node
list (five seconds while foregrounded), so a run that reaches Needs approval shows
within one refresh. A desktop that predates the route answers `{"error":"not found"}`
with `200`; the clients read that as "no circuit support", and a failed circuit read
never takes the node list offline.

`NodeCircuitRun` is derived by `db::circuit::node_run` and generated to
`src/types/generated/`. It carries the blueprint name, raw run state, the current step
with a plain role (`Review`, `Approval gate`), the review round and its limit, the
verdict and shortened report for the *current* pass only (a retry reuses the node id, so
an unmatched verdict belongs to an earlier pass), the history `revision`, and
`needs_you`. Each need has a kind:

| Kind | Meaning | What the phone offers |
|---|---|---|
| `approval` | An approval gate is parked (**Needs approval**) | **Approve…**, with a confirmation |
| `unverified` | Buildmesh cannot verify a step (**Unverified Checkpoint**) | Exactly the checkpoint actions the desktop run card lists for it |
| `input` | The agent raised a permission or question | **Open terminal to answer**; the answer goes through the existing terminal path |
| `failed` | The run failed at this step | The run's `recovery` options: **Retry this step**, **I've done this — continue** |

Eligibility is not recomputed for the phone. `unverified` actions and `recovery` come
from `evidence::attention_inner` and `step_recovery::build_recovery`, the functions the
desktop run card reads, and the act routes call the desktop commands' implementation.
So Run Recovery is not offered, and is refused if sent, for a review approval, a
pull-request lookup or a spawn (`recover_is_refused_for_a_failed_review_approval`). A
review verdict recorded from the phone requires written evidence, as on the desktop,
and is attributed to the person.

| Route | Does |
|---|---|
| `POST /api/circuit-runs/{id}/approve` `{node_id}` | Approve the parked gate (`approve_circuit_step`) |
| `POST /api/circuit-runs/{id}/outcome` `CheckpointRequest` | Recheck or record an Operator-recorded Outcome (`record_circuit_outcome`) |
| `POST /api/circuit-runs/{id}/recover` `RecoveryRequest` | Run Recovery (`recover_failed_circuit_run`) |
| `POST /api/circuit-runs/{id}/cancel` | Cancel and retire helper agents, as the desktop's Cancel does |

The run id is in the path; a body naming another run is `400`. A missing run is `404`.
A request the desktop refuses (a stale `expected_revision`, a step that already moved on,
a recovery that is not offered) is `409` with the desktop's own sentence. These routes
do not answer `401` or `403` themselves: the client reads those as a revoked device and
answers by dropping the session. Open sheets re-check the offered action against each refresh and close if it
is no longer offered, because approval has no revision of its own.

Not in this slice: starting a review from the phone, an attention-filter entry or event
socket message for "run needs you", the Android client, and authoring or editing circuit
graphs. Run history beyond the key events stays on the desktop.

## Session-cookie recovery

The session is the HttpOnly `bm_session` cookie, so JavaScript cannot read it and the
cookie cannot repair itself: when a browser loses that cookie while its device is still
paired, its requests start failing authorization and the phone used to answer by clearing
the credential and demanding a fresh re-pair (issue #1577).

Recovery needs a credential the client can actually re-present, and that is the paired
device token in `localStorage` (`buildmesh_token`). `POST /api/session` accepts it as a
bearer credential and re-mints the cookie from the **same** token. That route only validates
an existing `device_sessions` row (`routes::session::login`) — minting an identity is
`pairing::exchange`, reached from `/api/pair` alone — so a recovery refresh adds no device
row and cannot duplicate a session.

The transport in `src/mobile/api.ts` owns the whole policy, through two entry points:
`apiFetch` (every data route) and `fetchMintTicket`. The ticket mint cannot route through
`apiFetch` — it must hand a 429's `Retry-After` back to `mintWsTicketWithBackoff` — so it
joins the same recovery seam explicitly rather than skipping it, since the terminal and
events sockets are what a reconnecting phone opens first.

- **One refresh per burst.** Screens fan out several requests at once, so a lost cookie
  arrives as a burst of 401s. They join a single in-flight `POST /api/session` promise; each
  caller then replays its own request exactly once. A replayed cookie that still 401s means
  the device itself was revoked, so the retry is capped at one and the 401 propagates.
- **Only 401 recovers.** A 403 means the device authenticated and was refused for a scope it
  does not hold; no refresh can grant it. See "only a genuine authorization failure may reach
  `onAuthFailed`" above.
- **A refresh that reaches no verdict is not a rejection.** The refresh has three outcomes —
  refreshed, rejected, and unavailable (the desktop app unreachable, rate-limited or erroring).
  Only *rejected* lets a screen call `onAuthFailed`. *Unavailable* surfaces as an ordinary
  non-auth error, so the screen takes its usual offline/error path and the stored token
  survives: a network blip must not cost a phone a pairing that is still valid. This is the
  distinction that keeps "clear credentials only when the refresh is rejected" true rather
  than aspirational.
- **The transport never clears the credential.** It re-raises the 401 and the screen's
  `onAuthFailed` remains the single owner of the clear-and-show-Connect transition, so the
  screen-stays-mounted guarantee and the re-pair path cannot drift apart. The token rides one
  `Authorization` header on the refresh call and never a URL.

Credential lifetime follows from that: a successful `restoreSession` **keeps** the token
(it is the same device credential the cookie carries, and retaining it is what makes the next
cookie loss recoverable), a **rejected** refresh clears it, a network failure keeps it
(nothing is yet known about its validity), and a fresh `POST /api/pair` clears it because
pairing mints a new device identity that reaches the client only as a cookie.

One limit follows from the pairing design and is deliberate rather than a gap to close here:
`/api/pair` returns an empty body, so a device paired since the cookie migration (#500,
#1756) has **no** token in `localStorage` and nothing to recover with — it still needs a
re-pair. Recovery serves the devices that hold a token. Changing that means handing the SPA
a second, cookie-scoped credential, which is a server contract change, not a client one.

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
route validates text size. Whether the harness can take an initial prompt is
the rule in services::agent_node, shared with the desktop commands.
Mobile never races terminal startup by injecting the idea as keystrokes.

## Phone issue triage

Both phone clients filter open issues by title, number or label. Cards show
labels, author and blockers found in the loaded open feed; tapping a card
expands a bounded body preview and its GitHub link. Blockers outside the loaded
feed cannot be resolved. Feed completeness stays visible even when a filter
matches nothing, so a partial read does not imply that the repository is empty.

**Edit tags** lists repository labels and changes one label per acknowledged
request, preserving collaborators' other labels. Enabled issue-triggered
Autopilot circuits identify their watched tags in the editor. Adding one makes
the issue eligible on the next poll; dependencies and existing runs still apply.
GitHub failures retain the previous selection and allow retry. Repository label
load failures have a separate retry; older desktops without these routes need
an update to use the editor.

The admin-only `GET /api/meshes/{id}/issue-labels` returns the generated
`IssueLabelCatalog` (names and mesh-scoped enabled circuit trigger names).
`POST /api/meshes/{id}/issues/{number}/label` accepts `{label, present}` and awaits
the desktop's single-label command. GitHub write failures are HTTP 500, distinct
from HTTP 401/403 device authorization failures.

**Start agent** opens a launch configuration choice in both clients, with
**Mesh default** or an available Spawn Option supporting prefill. Saved launch
recipes are excluded because this route accepts only a Spawn Option. Starting
requires confirmation; cancellation creates no node, and the picker stays open
while a request is pending. Browser back first dismisses the web picker and is
consumed while it is starting.

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

