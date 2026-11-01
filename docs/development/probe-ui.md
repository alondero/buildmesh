# Probe panel, view modes, and context lenses

Status: current

## Canvas Layout (View Modes)
The canvas exposes five **View Modes** (wayfinder #982; state model #983; rendering #986; Filtered added #1609). The active mode is a pure UI string-literal union — no backend serialises it. `deriveScope` in `src/lib/viewModes.ts` is the pure definition of the effective scope: the View Mode, the anchoring Mesh or none, whether the scope is Mesh-scoped, the visible and pre-filter node counts, and the Mesh the focused node contributes. `resolveSingleNode` is the Single-mode fallback (the active node, then the first node of the grid Single was entered from, then any node).

React surfaces read that answer through `useScope` (`src/hooks/useScope.ts`). The hook is the only place that joins View Mode, the selected Mesh, and the active Agent Node for the scope question, and it delegates to those two functions instead of restating them. Keyboard traversal calls `readScope`, the same assembly against the current store snapshot. Mesh names are opt-in (`useScope({ meshNames: true })`) so a rename re-renders the title-bar scope indicator and not the terminal grid. Two rules that are not the grid scope live in the same module: `diffLensDiverged` closes a diff when the focused node or the selected Mesh leaves the lens the diff was opened on, and `selectedActivityMember` chooses which member a Node Activity card shows. Card selections stay a per-card subscription, so a utility toggle does not re-render every scope consumer. The grid render, the canvas empty state's counts, keyboard traversal (#987), the title-bar scope indicator, and unit tests therefore cannot disagree about the scope (#2071).

- **single** — Solo the active node; subsumes the old maximize toggle. Escape returns to the grid mode `single` was entered from.
- **mesh** — Scope to the sidebar-selected Mesh, and nothing else. There is no fallback chain: with no Mesh selected, Mesh Grid is an explicit empty state rather than a guessed Mesh, because silently showing the focused node's Mesh (or the first loaded Mesh) shows a scope the user never chose. Entering Mesh Grid with no Mesh selected opens the Mesh picker. Mesh selection is sticky — re-clicking the highlighted Mesh is a no-op while the canvas is already that Mesh's grid, and returns the canvas to that Mesh's grid from Pinned/Filtered/Single (#2072).
- **pinned** — Cross-mesh filter over `is_pinned`; deliberately never touches `selectedMeshId`.
- **all** — Every loaded node, across every mesh. All Nodes carries a one-way invariant: `viewMode === 'all'` requires `selectedMeshId === null`. `setViewMode` enforces this transition directly so callers remain side-effect-free.
- **filtered** — Cross-mesh view narrowed by the Grid Controls (free-text search + provider/status filters; the Search Nodes bar mounts in the title bar only while this mode is active).

The five-segment control is the bespoke `ViewModeSwitcher` in the title bar (below 1440px a single labelled `ViewSelector` names the View Mode and scope and holds both pickers, #2421); the adjacent scope indicator names that scope (the Mesh, or an honest cross-Mesh label carrying an Agent Node count) and doubles as the Mesh picker, so choosing a Mesh is one write that moves the canvas and the Mesh-owned Probe destinations together (#2074, #2077). A search that escapes Mesh scope, and deleting the Mesh the canvas was showing, announce the move instead of changing the view silently (#2076).

`src/lib/navigation.ts` owns desktop navigation across the mesh, agent activity,
View Mode and Probe stores. `focusNode` can keep the current grid, follow the
node into its mesh, or solo it; following a node from Single preserves the
selected grid and its return mode. `enterMesh` restores the chosen mesh grid,
optionally focusing a node and opening a Probe destination. `revealRun` selects
the owning mesh before requesting the run in Circuits. Store setters retain
local invariants, including All Nodes clearing mesh selection; mesh selection
no longer drives View Mode through a cross-store subscription. Lifecycle
operations load navigation lazily rather than making the mesh store import
the UI store during initialization. Mobile navigation is separate.

Opening is mirrored in the sidebar: a Mesh row expands when its Mesh becomes
the selected one, and when one of its agents becomes the active Agent Node, so
the agent a user just opened is visible in the list they opened it from. Both
signals are read on their rising edge — a newly opened agent, or a Mesh that
just became selected — by the row itself (`MeshItem`), which owns the
`activeNodeId`-keyed bit instead of taking a prop from `Sidebar`, the same seam
`NodeItem` uses for its own active bit (#1748). A collapse the user made by
hand therefore survives later re-renders and only a newly opened Mesh or agent
expands the row again.

## Probe adapters and scoped reads

For contributors changing a destination: add its icon, ownership metadata and
`probeTabAdapter` entry to `PROBE_TAB_DEFINITIONS` in
`src/lib/probeContext.ts`. The entry names the component and its read in one
dynamic import. Palette command copy and tool-discovery membership also live
in that entry. Destination IDs and order derive from the record; the panel
renders its component, and test preloading traverses its `preload` functions.
The adapter contains no rendering branches, and tab chunks remain lazy.

`src/lib/probeResource.ts` owns non-path read state. `useProbeResource` takes a
loader, load context and explicit subject key (including server-side filters).
A refresh token invalidates the same subject without discarding its data.
A subject change creates an empty resource and disposes the outgoing one.
Within a subject, only the newest live request can commit data, error or
completion. Reload retains that subject's last successful data; rejection keeps
it with an error. Reads whose old data would assert current eligibility use
`discardOnError`, which keeps that data absent through pending retries.
Disposal aborts the signal and drops late success/rejection,
even when IPC cannot cancel the backend operation. Returning to a subject starts
a fresh owner. `mutate` aborts older reads before applying an acknowledged write.
Manual busy feedback survives competing background reads until the latest read
settles. Optional polling waits until each read settles before arming its timer.
PR mergeability keeps its bounded retry budget and row disclosure rules in the tab.

Existing shared owners remain authoritative: Project Files and Agent Changes
route their registry reads through path-invalidated caches, settings and notes
use the Mesh editing session,
and Notifications reconciles through the notification store. Their registry
loaders delegate to those owners rather than introducing a second mutation
policy. The resource governs local read publication, not store-owned drafts or
cross-surface reconciliation. `tests/unit/probe-resource.test.tsx` exercises the
adapter contract for every destination in both completion orders, plus mutation,
error, refresh and disposal behavior.

## Probe Context Lenses (issue #1456)
Probe destinations have explicit ownership in `src/lib/probeContext.ts`; the
complete `PROBE_TAB_DEFINITIONS` record is the source of truth for ownership
lens,
baseline, selection-following, and statefulness. The three lenses are
`Host` (machine-wide provider/account/runtime state), `Mesh` (one repository,
its configuration, GitHub feeds, worktrees, automation, and notes), and
`Agent` (one Agent Node's changes and actions). Agent History is a Host finder
with an explicit repository filter and a separate discovered-session source. Notifications is a fixed Host destination across meshes, reached from the bell
immediately left of Zoom or the tool palette. Usage is
Host-lens and must never display or infer a Mesh name. Project Files is
Mesh-owned but may show a focused Agent Node's working tree; Agent Changes is
Agent-lens and uses the node-base baseline. The `useProbeContext` hook is the
read seam for the shell and destination tabs: use its subject, resolved IDs,
paths, and `hasRequiredContext` instead of reconstructing ownership from
`selectedMeshId` or `activeNodeId`. The Probe header follows the two-line
[design contract](../../DESIGN.md#affordances-and-progressive-disclosure);
the current subject stays visible at a legible weight, and the expandable scope
line exposes full names and selection-following behavior by pointer, touch or
keyboard. [ADR-0052](../adr/0052-probe-scope-progressive-disclosure.md) revises
the earlier permanent mode-label mandate. The context mode has exactly two values,
`fixed` and `following`: no
destination can hold a subject that disagrees with the sidebar selection, and a
subject that disappears renders the explicit unavailable-context state instead
of resolving a neighbour. See
[`docs/adr/0029-probe-context-lenses.md`](../adr/0029-probe-context-lenses.md)
for the destination mapping and mixed-ownership decisions, and
[`docs/adr/0041-disclosure-over-capture.md`](../adr/0041-disclosure-over-capture.md) for why ADR-0029's pinning mandate was reversed in favour of disclosure (issue #2073). Issue #1375 moved
Probe navigation title-bar-first — a command palette plus an on-demand
inspector with no rail — but the lens contract itself is unchanged; see
[`docs/adr/0030-titlebar-navigation-on-demand-inspector.md`](../adr/0030-titlebar-navigation-on-demand-inspector.md).
ADR-0032 added a working-set tab strip *inside* the open inspector
(`ProbeToolRail`, MRU-capped, ⊞ opens the ADR-0031 tool grid) for fast
alternation; it renders only while the panel is open and adds no reopen
affordance, so the closed-render discipline stands.

Related Files/Changes and Issues/Pull Requests share a strip slot, with
unchanged command IDs. The working-set reducer
owns group identity and replaces a group's remembered subview in place;
recency affects eviction only. The Files subview is persisted independently
of the session-only working set. Agent History reads all database nodes,
including archived rows; reopen restores Suspended without spawning and adopts
the returned row into the live store. See [ADR 0039](../adr/0039-desktop-audit-navigation-and-readiness.md).

Agent History's two modes — the Lifecycle list and the discovered-sessions list —
share one destination but not one toolbar. The Lifecycle mode owns the header's
Refresh; the discovered list is `ArchivedNodesTab`, which brings its own
`ProbeToolbar` and Refresh, so the header renders its Refresh only in Lifecycle
mode. The mode pair is a labelled segmented group whose active member owns a
background and text colour, so the current mode is visible rather than
inferred, and the repository filter renders only when it has an option to
offer. Card timestamps
use `formatListTimestamp` (`src/lib/time.ts`) rather than the stored RFC 3339
value, which carries Rust nanosecond precision.

Routing defaults use `list_routing_options`, a preferences/cached-installation
catalog that does no subprocess or WSL probing. Unverified OpenAI routes stay
disabled pending the live provider menu. Settings owns separate preference,
routing and probe request sequences; account, route and Launch Configuration
mutations refresh both routing sources. Anthropic routes share the live menu's
pure launchability predicate, and configuration-specific errors retain their
remediation. The cheap catalog reads the default WSL distribution only if
startup has already observed it; it never initializes discovery itself.
Failed preferences disable writes, and retries/unmount invalidate stale reads.

## Notifications

The Notifications Probe puts actionable problems before recent toast activity.
`toastStore.addToast` captures history in the same transition as the visible
toast, before the three-toast cap — unless the caller asks for a silent record
(one summary toast instead of one per problem). Dismissal and expiry affect the
toast stack only. History and read state persist locally; the newest 100
ordinary or resolved records are retained alongside every unresolved actionable
problem of any kind. Mark all read and Clear read preserve unresolved problems.
The bell prioritizes an amber count of unresolved problems — blocked worktree
cleanups and incomplete mesh syncs alike — otherwise an unread count. Storage
failures retain the in-memory view.

Worktree failure events record an actionable notification without opening the
recovery dialog. Resolve refreshes the live queue and opens the existing cleanup
dialog with the selected worktree first. Definitive Retry/Keep replies update
the notification with their distinct outcomes; failed retries and failed restores
retain the problem. Startup, 30-second polling, opening the destination and
manual Refresh reconcile against the durable queue, restoring failures after
restart and following automatic recovery. Reads are owned by the latest request
and fenced by a worktree revision so an older snapshot cannot hide a newer
failure or resurrect a manual resolution. A read failure preserves last known
problems and shows a retryable alert outside the scroller. Queue absence means
only that cleanup is no longer pending, without claiming how it ended.

Needs attention holds two kinds of unresolved problem, each with its own card
and its own recovery, because the two problems are unrelated and are ended in
different ways. A **blocked worktree cleanup** is the queue-backed kind: only the
reconciliation may close it, so that inference is confined to
`worktree-cleanup` actions and never touches another kind. A **mesh sync that did
not complete** is the transient kind: `lib/meshSyncFeedback` classifies
`GitSyncResult.status` into a toast severity, and `Partial`, `Unverified` and
`Failed` — the three statuses that leave the mesh's position unresolved or
unknown — raise an item whose Retry button re-runs the sync through the same
path the sidebar uses. `Skipped` (no remote) and `NoUpstream` (a branch that
tracks nothing) do neither: both are ordinary states that raise nothing, and
neither resolves an earlier failure, because neither verified anything. Only a
result that *verified* the mesh (`Synced`, `UpToDate`) closes its card, and its resolution deliberately does
not bump the worktree revision, because it is not newer evidence about the queue.
Both kinds deduplicate by action kind *and* path, since a mesh's root checkout
can itself be a blocked worktree path.

The sync card's outcome is read back by the sidebar rather than held in it. Each
sync publishes two independent signals per mesh path — whether the mesh was
verified, and whether the cached behind-count may be stale — and a mesh row
applies each to the piece it owns: verification clears the failure badge, a
possibly-stale count re-reads the branch status. The second signal is
deliberately "may have changed", not "did": the manual fetch runs without
`--atomic`, so it can update some remote-tracking refs before failing on
another, leaving a `failed` result's count possibly stale. It is also true for a
`failed` outcome that never launched a fetch at all (an unusable repository),
because one redundant read is cheaper than trusting a flag that cannot make
that promise. Only a mesh with no remote — where no
fetch was launched at all — skips the refresh, so that publication is reserved
for a provable no-op. The split is what a `Partial` run needs: it advances the
remote-tracking ref without fast-forwarding, so the count is stale while the
badge is correct. Both signals are applied by the same effect whether the sync
was started from the row, from this probe's Retry button, or from the omnibar's
bulk command — the bulk path publishes for the same outcomes the single-mesh
path does, so the two cannot drift apart.

What the card may *claim* is a third thing again, and the distinction is
recorded per notification as `evidence`. A `partial` run establishes that the
mesh is behind, so the card says so. Everything else establishes nothing either
way — a fetch that did not complete, a comparison that could not be made, an
IPC that rejected, a record written before the field existed — and the card
says the state is unknown rather than choosing a reading. Note what is *not* a
claim: `fetched: false` means the fetch did not complete, and it cannot say
whether `git fetch` reached upstream, died mid-fetch, or was never spawned — so
it settles nothing either way. Only `skipped` — no remote at all — proves
nothing was fetched, and that one raises no card to make the claim.

A command that syncs many meshes at once records its per-mesh problems silently
— they land in Needs attention and nowhere else — and shows one summary toast,
because N problem toasts would overflow the three-toast stack. That summary
names the two kinds of nothing-to-do too, and keeps them apart: `no remote` and
`nothing to compare` are different facts, and a mesh that has a remote must not
be told it has none.

The destination uses the standard Probe shell, tool rail and 240–720px resize
bounds. Its toolbar and footer stay pinned around one inner scroller; diagnostic
text and action rows wrap at the minimum width. The recovery modal is loaded
after explicit Resolve.

## Configuration vs maintenance destinations (issue #1460, ADR-0038)
Two Mesh-lens destinations are split by job and must never merge again:
`properties` (**Project Settings**) owns configuration, `worktrees`
(**Repository**) owns maintenance. Project Settings is the only home for
identity/directory, agent runtime defaults, build and run, and worktree strategy
(use-worktree, base ref, mode, warm pool, worktree directory) — the last group
moved there from Repository. Use-worktree is the group's gate, and a **new**
Mesh's value for it is decided at creation, not by this form: every
mesh-creation entrypoint in `commands/mesh` (`add_mesh`, `create_mesh`, and
`clone_mesh_repo`) asks `git::project::can_cut_worktrees` — the same resolution
`create_git_worktree` requires — and writes `meshes.use_worktree` accordingly,
so a Mesh over a folder with no repository cannot promise a worktree it cannot
cut. The detection is fs/git I/O and runs **before** the row is written,
because `db::create_mesh*` holds the writer connection; a worktree-disabled row
is created with `pre_spawn_pool_size = 0` (the v24 backfill's invariant). The
form is still where a user changes it, and only ever shows the stored value.
Repository keeps
health/recovery, branch and
worktree cleanup, and remote-tracking prune, and imports no `updateMesh*`
wrapper. Both render labelled `ProbeSection` groups, both state that they act
on the project root rather than a focused Agent Node's worktree, and Delete Mesh
lives in a `tone="danger"` section that states impact and recovery before the
shared `ConfirmDialog` repeats the scope. The `probe-<tab>` **ids are
deliberately unchanged** (ADR-0030 keeps them stable for callers and deep links)
even though the source files are named after the destinations; the palette
reaches both through the `Project` tool group and the title bar through one
`More` disclosure. Neither destination takes permanent navigation space.

The Issues and Pull Requests probes are both Mesh-owned GitHub feeds. Their
backend is split by resource under `services::github`: `issues` owns issue
listing, Blocked-by parsing, and trigger-label flags; `prs` owns pull-request
listing, merge strategy, and contributor data; `sync` owns the host token,
HTTP timeouts, and rate-limit classification (a rate-limit body stays an API
error, not a missing repository). `refresh_decision` and
`combine_live_and_cache` live in `sync` as the shared TTL, coalescing, and
live-over-cache rules both probes must use if a snapshot store is added;
live list methods fetch every time today. `services::github` re-exports the
command-facing types, so handlers keep calling `services::github::...`.
Issue-only parsing stays in `issues`; pull-request merge logic stays in
`prs`. Repository lookup in `commands::pr` uses the shared host-path opener.
Unreadable repositories propagate errors to both feeds; only readable repos
without a GitHub origin produce an empty list. WSL ownership trust is an exact
`safe.directory` entry in Windows Git configuration, independent of GitHub
authentication (see [troubleshooting](../troubleshooting.md#github-feeds-fail-for-a-wsl-mesh)).

## Configuration saves reject, they never resolve silently

Every Project Settings field saves through `meshSettingsStore`'s `save`, which routes
success and failure to the destination's `SaveIndicator`. That is only meaningful if the
store action underneath actually rejects: `meshStore.updateMeshName` used to catch the
`update_mesh_name` IPC failure, write `state.error`, and **resolve**, so the save channel
took its success branch and the tab rendered "Saved" for a write the backend refused —
while the sidebar, which only moves on success, kept the old name. The action now
rethrows after recording the error, so the caller shows the real rejection and the
draft the user typed survives for a retry. The pattern generalises: a store action
that swallows a failure makes every success indicator above it a lie.

## Per-Mesh editing session (issue #2242)

`meshSettingsStore` is the single owner of the editable state the two Mesh-lens
configuration destinations render: Project Settings' form and worktree-strategy drafts,
the Scratch Pad's text, the save-status machine, and the load that fills them. The tab
files keep only what is local to the render (provider catalogue, the app-wide default
label, the sandbox-availability probe, the `gh auth` flag, the delete confirmation) and
render the session, so the destinations stay dumb and the state has one writer.

Three rules live in the store because each one is about the *session*, not a widget:

- **Load once per Mesh, applied only while it is the newest request.** One
  `get_mesh_properties` feeds the form, the worktree strategy, and the directory
  resolution; detection, preferences and the directory config ride along as
  individually best-effort reads so a failure degrades the "Effective:" display rather
  than blanking the form. Identity is a monotonic token rather than a Mesh-id
  comparison, so a retry or a path edit retires the previous read too. Drafts are
  cleared when a load *starts*, not when it resolves, so a slow load cannot leave
  the previous Mesh's values in the form under the new Mesh's header — the user would
  edit one project and save it to another. This is the same authority rule
  `useMeshStore` applies to `list_meshes` (below).
- **A save that settles after the user left drops its result.** Each save captures the
  session's Mesh when the IPC starts and compares it on settle. A "Saved" or a "Save
  failed" for the outgoing Mesh describes a project the user is no longer looking at.
  The Scratch Pad applies the same rule with its own revision counter, because a second
  keystroke also supersedes the first write's result.
- **One indicator per session, not per field.** `idle → saving → saved` auto-clears
  after a fixed window; `error` persists until the next save starts. Worktree-strategy
  failures use a section-local channel instead, because that message has to name the
  column that refused the write.

Because every async continuation writes to the store rather than into a component's
`useState`, a write that settles after a destination unmounts is harmless. That is what
the hand-rolled `mountedRef` and `activeMeshIdRef` in `ProjectSettingsTab` used to
guard, and what `ScratchpadTab`'s own `AbortSignal` ref guarded as a second copy: the
store is the owner of "which Mesh am I editing", so the mirrors are gone rather than
relocated.

## Mesh snapshot authority (issue #2326)

`useMeshStore` owns one authority policy for the Mesh collection, so a caller can ask
for a refresh without knowing which sidebar action, capacity editor or deletion is
running. A `list_meshes` response may replace `meshes`/`meshesById` only while it is
still the newest thing the store knows; three things can take that authority away
while a read is in flight, and each is observed inside the store rather than in the
caller:

- **Request identity.** Overlapping `fetchMeshes`/`refreshMeshes` callers each get
  their own read, and each response may commit only while it is newer than the
  response already accepted. Resolving them in either order therefore leaves the
  newer snapshot in place — the review's reproduction (older A resolved after newer
  B) used to leave the *older* one.
- **Mutation authority.** Every committed local write advances a collection
  revision: create/add/clone, the delete commit, rename, colour, layout, and the
  optimistic reorder. A read that observed an older revision is describing a
  database state the user has already moved past, so an acknowledged Mesh setting
  can no longer flicker back to the value it replaced. `deleteMesh` advances it
  after the backend commits and before its refetch, so no late snapshot can put the
  deleted row back.
- **Pending optimistic reorder.** A drag's order is deliberately ahead of the
  persisted order until `update_mesh_positions` acknowledges it, so no snapshot may
  replace it — not even one started after the drag.

An obsolete response is dropped rather than applied, and dropping never replaces a
stale-write bug with permanently stale state: a trailing authoritative read converges the
collection whenever a committed write no applied snapshot reflects yet, shared by every
response that same newer write made obsolete. A read dropped for any other reason needs no
such refresh — it lost to a *newer read* under an unchanged revision, so that newer
snapshot already saw everything it would have, and a third read would be IPC and loading
churn for nothing. A trailing read may re-arm exactly one further refresh if a second
write lands while it is in flight, and no more — convergence must not become a refresh
loop under a continuous stream of writes. It never runs during a reorder window (where it
would be dropped the same way): a successful reorder converges it and a failed one
refetches. `loading` follows the number of reads still outstanding, so a dropped response
cannot mark a newer read as finished, and a failure only reaches `error` when it is still
authoritative: an old rejection never replaces a newer success, and a newer snapshot that
applies later clears an older failure on its way in rather than leaving an error banner
over fresh data.

`refreshMeshes` rejects for callers that must confirm the refresh (boot readiness,
`CircuitRunCapacity`), and `fetchMeshes` records the failure without throwing. A read
that was *superseded* resolves rather than rejects — it has no failure of its own to
report and its snapshot was never applied — and `refreshMeshes` follows the trailing
read it started so it resolves on an applied snapshot. One reconciliation builds both
collection views from the same rows and keeps the previous object reference for a row
whose fields compare equal, so `meshes` and `meshesById` cannot disagree and per-id
selectors (`s.meshesById.get(id)`) skip a render when only other Meshes changed.
`agentNodeStore` owns the analogous agent-node policy, including coalesced refreshes
and its snapshot revision fence.

## GitHub list completeness

Every GitHub list read goes through one paginator,
`src-tauri/src/services/github/pagination.rs` (issue #1528). It follows the RFC 8288
`Link` header's `rel="next"` rather than a hand-rolled `?page=N`, and it backs both the
REST list endpoints and the `/search/issues` envelopes used by Autopilot and Circuit
ingest. Three rules are load-bearing:

- **A failed page fails the whole read.** Page 1 succeeding is not evidence the read is
  complete, so a 500 on page 2 returns `Err`; it never returns page 1 as a success.
- **Truncation is data, not silence.** Each walk returns a `Page<T>` carrying
  `GitHubPageCompleteness` (`returned`, `pages_fetched`, `complete`,
  `incomplete_reason`, `reported_total`), generated into
  `src/types/generated/`. `incomplete_reason` is `safety_cap` (the page budget ran out),
  `search_ceiling` (GitHub's search API caps at 1,000 matches), `upstream_incomplete`
  (GitHub set `incomplete_results`, or the `Link` chain ended before its own
  `total_count`), or `cancelled`.
- **Reconciliation ingest refuses a partial feed.** `require_complete_read` turns any
  incomplete read into `GitHubError::Incomplete`, because a silently shortened trigger
  set means a labelled issue or PR on page 2 never gets a run. Read-only consumers get
  the `Page` and show the gap instead.

The three read-only commands return a **wrapper, not a bare array** —
`GitHubIssueFeed`, `GitHubPullRequestFeed` and `PrFileFeed`, each `{ items, completeness }`
(generated into `src/types/generated/`). A `Vec` made "page 1" and "everything"
indistinguishable, which is the whole defect; the wrapper is what lets a caller tell them
apart. `FeedCompletenessNote` renders the gap in the Issues tab, the Pull Requests tab and
the PR file list, and names the *reason* because the remedies differ: a safety cap or a
search ceiling is a permanent limit of the read, while upstream incompleteness or a
cancellation is transient and worth retrying. A mesh with no GitHub remote reports
`complete: true` — the read never happened, so there is nothing to be incomplete about,
and the existing empty state must not turn into a truncation warning. The mobile
`GET /api/meshes/{id}/issues` route serialises the same wrapper, so `listIssues` reads
`.items` off it.

The walk is bounded and cancellable: `PaginationPolicy::DEFAULT` is 10 pages × 100 =
1,000 items, matching GitHub's own search ceiling, and both the budget and
`GitHubClient::cancel` are checked *before* each request, so a stopped read costs no
further API calls. Ordering is server order across concatenated pages, deduplicated by
identity so an item created between two requests appears once.

The PR-summaries path uses GraphQL cursors rather than `Link`, but reports through the
same `GitHubPageCompleteness` and shares the same budget.

## Probe Panel shell (scroll ownership + narrow width)
The panel and keyed destination wrapper are layout-only, with `min-h-0`,
`min-w-0` and `overflow-hidden`. A destination owns one inner
`flex-1 min-h-0 min-w-0 overflow-y-auto overflow-x-hidden` body; the shared
`ProbeTabBody` supplies this contract. Toolbars and recovery controls are
`shrink-0` siblings. A bounded error excerpt may scroll separately, but must
leave Retry visible. At the dock's 240px minimum, unbounded prose wraps and
unspaced paths/errors use `break-all`. Stating only vertical overflow computes
horizontal auto overflow. Keep Notes' mesh-aware save/restore ownership.

