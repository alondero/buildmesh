# Probe panel, view modes, and context lenses

Status: current

## Canvas Layout (View Modes)
The canvas exposes five **View Modes** (wayfinder #982; state model #983; rendering #986; Filtered added #1609). The active mode is a pure UI string-literal union — no backend serialises it. Every surface that must answer "what is the user looking at, and which Mesh is it" reads one pure, store-independent derivation instead of re-deriving scope for itself: `deriveScope` in `src/lib/viewModes.ts` resolves the effective scope once — the View Mode, the anchoring Mesh or none, whether the scope is Mesh-scoped, the visible and pre-filter node counts, and the Mesh the focused node contributes. The grid render, the canvas empty state's counts, keyboard traversal (#987), the title-bar scope indicator, and unit tests all consume that single value, so no two of them can disagree about the scope (#2071).

- **single** — Solo the active node; subsumes the old maximize toggle. Escape returns to the grid mode `single` was entered from.
- **mesh** — Scope to the sidebar-selected Mesh, and nothing else. There is no fallback chain: with no Mesh selected, Mesh Grid is an explicit empty state rather than a guessed Mesh, because silently showing the focused node's Mesh (or the first loaded Mesh) shows a scope the user never chose. Entering Mesh Grid with no Mesh selected opens the Mesh picker. Mesh selection is sticky — re-clicking the highlighted Mesh is a no-op while the canvas is already that Mesh's grid, and returns the canvas to that Mesh's grid from Pinned/Filtered/Single (#2072).
- **pinned** — Cross-mesh filter over `is_pinned`; deliberately never touches `selectedMeshId`.
- **all** — Every loaded node, across every mesh. All Nodes carries a one-way invariant: `viewMode === 'all'` requires `selectedMeshId === null`. `setViewMode` enforces this transition directly so callers remain side-effect-free.
- **filtered** — Cross-mesh view narrowed by the Grid Controls (free-text search + provider/status filters; the Search Nodes bar mounts in the title bar only while this mode is active).

The five-segment control is the bespoke `ViewModeSwitcher` in the title bar; the adjacent scope indicator names that scope (the Mesh, or an honest cross-Mesh label carrying an Agent Node count) and doubles as the Mesh picker, so choosing a Mesh is one write that moves the canvas and the Mesh-owned Probe destinations together (#2074, #2077). A search that escapes Mesh scope, and deleting the Mesh the canvas was showing, announce the move instead of changing the view silently (#2076).

## Probe Context Lenses (issue #1456)
Probe destinations have explicit ownership in `src/lib/probeContext.ts`; the
complete `PROBE_TAB_DEFINITIONS` record is the source of truth for ownership
lens,
baseline, selection-following, and statefulness. The three lenses are
`Host` (machine-wide provider/account/runtime state), `Mesh` (one repository,
its configuration, GitHub feeds, worktrees, automation, and notes), and
`Agent` (one Agent Node's changes and actions). Agent History is a Host finder
with an explicit repository filter and a separate discovered-session source. Usage is
Host-lens and must never display or infer a Mesh name. Project Files is
Mesh-owned but may show a focused Agent Node's working tree; Agent Changes is
Agent-lens and uses the node-base baseline. The `useProbeContext` hook is the
read seam for the shell and destination tabs: use its subject, resolved IDs,
paths, and `hasRequiredContext` instead of reconstructing ownership from
`selectedMeshId` or `activeNodeId`. The Probe header labels the subject and
whether the destination is fixed to Host or following selection; that label is
promoted to a legible weight because naming the subject is the whole anti-drift
guarantee. The context mode has exactly two values, `fixed` and `following`: no
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

Routing defaults use `list_routing_options`, a preferences/cached-installation
catalog that does no subprocess or WSL probing. Unverified OpenAI routes stay
disabled pending the live provider menu. Settings owns separate preference,
routing and probe request sequences; account, route and Launch Configuration
mutations refresh both routing sources. Anthropic routes share the live menu's
pure launchability predicate, and configuration-specific errors retain their
remediation. The cheap catalog reads the default WSL distribution only if
startup has already observed it; it never initializes discovery itself.
Failed preferences disable writes, and retries/unmount invalidate stale reads.

## Configuration vs maintenance destinations (issue #1460, ADR-0038)
Two Mesh-lens destinations are split by job and must never merge again:
`properties` (**Project Settings**) owns configuration, `worktrees`
(**Repository**) owns maintenance. Project Settings is the only home for
identity/directory, agent runtime defaults, build and run, and worktree strategy
(use-worktree, base ref, mode, warm pool, worktree directory) — the last group
moved there from Repository. Repository keeps health/recovery, branch and
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

Every Project Settings field saves through `wrappedSave`, which routes success and
failure to the destination's `SaveIndicator`. That is only meaningful if the store
action underneath actually rejects: `meshStore.updateMeshName` used to catch the
`update_mesh_name` IPC failure, write `state.error`, and **resolve**, so `wrappedSave`
took its success branch and the tab rendered "Saved" for a write the backend refused —
while the sidebar, which only moves on success, kept the old name. The action now
rethrows after recording the error, so the caller shows the real rejection and the
draft the user typed survives for a retry. The pattern generalises: a store action
that swallows a failure makes every success indicator above it a lie.

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

**Wire shape still to close.** `get_repo_issues`, `get_repo_pulls`, and `get_pr_files`
return bare arrays today, so the completeness metadata the backend now produces is
dropped at the command boundary and the probe tabs cannot yet render a
"showing first N of M" banner. The paged variants that carry it —
`list_issues_only_paged`, `list_pr_summaries_paged`, `list_pr_files_paged` — exist and
are tested; wiring them through `commands::pr` into a `{ items, completeness }` return
is the remaining step.

## Probe Panel shell (scroll ownership + narrow width)
The panel and keyed destination wrapper are layout-only, with `min-h-0`,
`min-w-0` and `overflow-hidden`. A destination owns one inner
`flex-1 min-h-0 min-w-0 overflow-y-auto overflow-x-hidden` body; the shared
`ProbeTabBody` supplies this contract. Toolbars and recovery controls are
`shrink-0` siblings. A bounded error excerpt may scroll separately, but must
leave Retry visible. At the dock's 240px minimum, unbounded prose wraps and
unspaced paths/errors use `break-all`. Stating only vertical overflow computes
horizontal auto overflow. Keep Notes' mesh-aware save/restore ownership.

