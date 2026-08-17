/**
 * GitIssuesTab — the Probe Panel's Git Issues tab body (issue #378).
 *
 * Thin wrapper port of the legacy `GitHubIssuesModal` (issue #113). The
 * dock supplies the header, mesh-name subheading, and close button, so
 * this component drops the modal's backdrop / header / Escape handler
 * and renders the same list + split-spawn button in the probe's 360px
 * body.
 *
 * Both call sites use the same backend-owned acceptance path:
 *
 *   1. `create_issue_node` — commits a `pending` row and starts the intent-
 *      driven launch in the background.
 *   2. `node-spawn-completed` / `node-spawn-failed` store listeners mirror
 *      the backend lifecycle transition.
 *
 * The user sees the dock-close → node-appear transition in well under
 * 500ms instead of the 5-10s they used to wait for the old synchronous
 * `spawn_issue_agent`.
 *
 * Blocked-by indicator (issue #481 follow-up)
 * -------------------------------------------
 * Below each issue's split Spawn button we render a small red flag when
 * the issue's `blocked_by` list contains at least one number that's still
 * in the loaded open-issues set. The cross-reference is frontend-side
 * (no extra API call) so the indicator naturally tracks GitHub state
 * via the existing `get_repo_issues` refresh — closed blockers disappear
 * from the flag the next time the list reloads.
 *
 * Known limitations (out of scope for v1, all documented in the plan):
 *   - Cross-repo blockers aren't detected (we only cross-reference
 *     against this repo's loaded open issues).
 *   - Pagination: `list_issues_only` caps at 100 open issues per page,
 *     so a blocker on page 2+ of a large repo may be missed.
 *   - "Blocks" (reverse direction) is not surfaced — GitHub's issue
 *     editor supports both "Blocked by" and "Blocks" sections, but v1
 *     only handles the former.
 */

import { formatError } from '../../lib/errorUtils';
import { useState, useMemo, useCallback } from 'react';
// `openUrl` is intentionally imported here even though the title + ↗
// links now route through `<SafeLink>`. The blocked-by button (further
// down in this file) is a `<button>`, not an `<a>`, so SafeLink
// doesn't apply — the button calls `openUrl(firstBlockerUrl)` directly
// to route the click through the OS (Tauri 2 WebView drops
// `target="_blank"` without an explicit capability we don't grant).
// Don't remove this import on a future "cleanup" pass.
import { openUrl } from '@tauri-apps/plugin-opener';
import {
  getRepoIssues,
  setIssueLabel,
  createIssueNode,
  listProviders,
  type GitHubIssue,
} from '../../lib/tauri';
import { useMeshStore } from '../../stores/meshStore';
import { addToast } from '../../stores/toastStore'; // Issue #1001
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useToggleSet } from '../../hooks/useToggleSet';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { useMeshGitHubUrl } from '../../hooks/useMeshGitHubUrl';
import { mapBackendProviders, type SpawnOption } from '../../lib/groups';
import { SpawnButtonCluster } from '../Sidebar/SpawnButtonCluster';
import { ProbeRow } from './ProbeRow';
import { ProbeTabBody } from './ProbeTabBody';
import { ProbeToolbar } from './ProbeToolbar';
import { SafeLink } from '../shared/SafeLink';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import {
  EmptyState,
  ErrorState,
  LoadingState,
  RefreshControl,
} from '../shared/Spinner';

/**
 * Build the human-readable tooltip text for the blocked-by flag.
 *
 * - If `blockers` is empty, return null (caller shouldn't render the flag).
 * - The first blocker is named in full (`#N — Title` when the title can
 *   be resolved from the loaded issues list, otherwise just `#N`).
 * - The remaining blockers are folded into a `+ N more` suffix so the
 *   flag stays a compact 12×12 SVG regardless of dependency depth.
 *
 * Module-local — the tooltip text is asserted through the rendered
 * DOM (`flag.title`) in `tests/unit/git-issues-tab.test.tsx`, so an
 * export here would broaden the component's public surface for no
 * test value.
 */
function buildBlockedByTooltip(
  blockers: number[],
  issuesByNumber: Map<number, GitHubIssue>,
): string | null {
  if (blockers.length === 0) return null;
  const [first, ...rest] = blockers;
  const firstIssue = issuesByNumber.get(first);
  const firstLabel = firstIssue ? `#${first} — ${firstIssue.title}` : `#${first}`;
  if (rest.length === 0) return `Blocked by ${firstLabel}`;
  return `Blocked by ${firstLabel} (+ ${rest.length} more)`;
}

export function GitIssuesTab() {
  const { activeMeshId, activeMeshPath } = useProbeContext();
  // `getDefaultProvider` is mesh-scoped — the only call that needs the
  // meshId directly, since it resolves the per-mesh > app-wide > default
  // precedence chain server-side.
  const getDefaultProvider = useMeshStore((s) => s.getDefaultProvider);

  const [issues, setIssues] = useState<GitHubIssue[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [spawning, setSpawning] = useState<number | null>(null);
  // Issue #979 — trigger-label toggle state. Two pieces (a third,
  // `toggleError`, was retired with issue #1001 — failures now surface
  // through the shared toast pipeline instead of inline state):
  //   1. `pendingToggle`: issue numbers with an in-flight `set_issue_label`
  //      IPC. Used to disable the badge so a second click can't race.
  //   2. `optimisticLabels`: per-issue label list override applied while
  //      the toggle is in flight. Cleared on success/revert; the source
  //      of truth (`issues[i].labels`) shows through after.
  const [pendingToggle, setPendingToggle] = useState<Set<number>>(() => new Set());
  const [optimisticLabels, setOptimisticLabels] = useState<Map<number, string[]>>(() => new Map());
  // Issue #1140 (T2.3) — destructive 'remove' used to call
  // `window.confirm`, which is the native browser chrome and breaks
  // the app's design language. Lift the gate into a ConfirmDialog so
  // the prompt matches WorktreeManager's delete confirm. `add` is
  // idempotent on GitHub so it skips the dialog entirely.
  const [confirmRemoveFor, setConfirmRemoveFor] = useState<number | null>(null);
  // Bump to force the load effect to re-run on a manual Refresh click
  // (issue #813 — Git Issues/PRs/Archive previously had no manual
  // refresh button). Mirrors the pattern `GitPullRequestsTab` already
  // uses for refetching after a successful merge: bumping the key
  // aborts the previous effect (useAsyncEffect's signal) and re-fires
  // the IPC, so a stale in-flight load can't clobber the refreshed
  // result.
  // Bump to refetch on manual Refresh (issue #813 — `useAsyncEffect`
  // aborts the previous effect's signal on dep change, so an
  // in-flight first-load can't clobber the refreshed result).
  const [reloadKey, setReloadKey] = useState(0);  // Only one dropdown open at a time, keyed by issue number — mirrors the
  // SessionBrowserModal pattern so the click-outside handling stays simple.
  const [openDropdown, setOpenDropdown] = useState<number | null>(null);
  // The provider list is fetched once at mount and reused for the lifetime
  // of the tab. Re-fetching on each open would be wasteful, and the list is
  // stable for the duration of a session (adding a new provider requires
  // an app restart).
  const [providerList, setProviderList] = useState<SpawnOption[]>([]);
  // Per-row expand state for the issue body. Set keyed by issue number
  // (not a single boolean) so cross-referencing two long issues stays
  // possible — the dock is 360px wide, but a user can scroll it freely.
  // Cleared on mesh change in the load effect below. `useToggleSet`
  // (issue #463) bundles the Set state + toggle closure + clear
  // reset into one hook so the load effect can call `expanded.clear()`
  // instead of `setExpanded(new Set())`.
  const expanded = useToggleSet<number>();

  // Cross-reference index for the blocked-by indicator. Built once per
  // render of the loaded open issues list — both as a Set (for fast
  // membership tests in the row render) and a Map (for resolving the
  // blocker number → title in the tooltip text). First paint has an
  // empty index because `issues` is `[]` until the IPC resolves, so
  // every row's `stillBlockedBy` collapses to `[]` during loading.
  // This is documented in the plan and pinned by the "first paint hides
  // flags" test — the alternative ("show flag whenever blocked_by is
  // non-empty") would be incorrect because we genuinely can't tell
  // whether a blocker is still open without the loaded set.
  const issuesByNumber = useMemo(
    () => new Map(issues.map(i => [i.number, i])),
    [issues],
  );

  // "View on GitHub" header button — resolves the active mesh's
  // `origin` to a `https://github.com/{owner}/{repo}/issues` URL.
  // `null` for non-GitHub meshes falls through to SafeLink's inert
  // `<span>` (no link, no dead click). The hook's dual-key cache
  // dedupes the IPC across mount + mesh switches within the session.
  const { url: githubUrl } = useMeshGitHubUrl(activeMeshId, activeMeshPath);
  const issuesListUrl = githubUrl ? `${githubUrl}/issues` : '';

  useAsyncEffect((signal) => {
    if (activeMeshId === null) return;
    const load = async () => {
      try {
        const result = await getRepoIssues(activeMeshId);
        // The mesh could have changed between opening the modal and the
        // IPC returning — drop the result in that case rather than
        // showing issues for a mesh the user no longer has focused.
        if (signal.aborted) return;
        setIssues(result);
        // Issue numbers are mesh-scoped — a row expanded in the prior
        // mesh would either be a no-op or accidentally open an
        // unrelated row in the new mesh. Clear on every mesh change.
        expanded.clear();
      } catch (e) {
        if (signal.aborted) return;
        console.error('Failed to load issues:', e);
        setError(formatError(e));
      } finally {
        if (!signal.aborted) setLoading(false);
      }
    };
    setLoading(true);
    setError(null);
    load();
  }, [activeMeshId, reloadKey]);

  // Fetch the provider list once at mount. Platform filtering (e.g.
  // macOS-only Anthropic) is enforced server-side via
  // AgentProvider::available_on(). Re-fetching on every render would
  // be wasteful, but the list can change during a session when the user
  // adds or removes a custom provider in App Settings — the hook below
  // re-fires this fetch on the `provider-list-changed` event so the spawn
  // picker drops stale accounts without an app restart.
  // Issue #575 / ADR-0016 — preserve the Spawn Option shape so the
  // `ProviderDropdown` can render the harness-grouped, always-expanded
  // menu (harness header + indented Proxied children). The 8-field
  // projection lives in `mapBackendProviders` (issue #583 cleanup).
  const refreshProviderList = useCallback(() => {
    listProviders()
      .then(backendProviders => setProviderList(mapBackendProviders(backendProviders)))
      .catch(err => console.error('listProviders failed:', err));
  }, []);

  useAsyncEffect(() => { refreshProviderList(); }, [refreshProviderList]);
  useProviderListInvalidation(refreshProviderList);

  // Close the provider dropdown when clicking outside of it. The dropdown
  // container carries a `data-dropdown-for` attribute set to the issue number.
  // Issue #492 — shared `useClickOutside` hook replaces the hand-rolled
  // add/removeEventListener pair; the hook pins the scoped selector so a
  // future caller can't reintroduce the loose-selector drift from Sidebar.
  useClickOutside(openDropdown, () => setOpenDropdown(null));

  // One backend-owned acceptance call. `create_issue_node` commits the
  // `pending` row and starts the intent-driven launch in the background.
  // The dock stays open after a successful spawn so the user can fire off
  // another issue without re-opening the context-menu → "Open Issues"
  // route. The node flips to 'running' (or 'error' on failure) via the
  // `node-spawn-completed` / `node-spawn-failed` store listeners. The
  // user dismisses the dock with the activity-bar toggle (or by
  // switching to a non-issues tab) when they're done.
  const handleSpawn = async (issue: GitHubIssue, providerId: string) => {
    if (activeMeshId === null) return;
    setSpawning(issue.number);
    try {
      await createIssueNode(activeMeshId, issue.number, issue.title, providerId);
      setOpenDropdown(null);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn issue agent:', e);
      setSpawning(null);
    }
  };

  // Primary "Spawn" button uses the mesh's resolved default provider —
  // explicit > per-mesh > app-wide > "anthropic" fallback is enforced
  // server-side by resolve_default_provider when we pass `provider`.
  // We mark `spawning` BEFORE awaiting getDefaultProvider so the split
  // button's `disabled` immediately blocks a second click on the same
  // issue (e.g. picking a different provider in the still-open dropdown)
  // from racing with the in-flight default-resolution IPC. If
  // `getDefaultProvider` rejects, the catch clears `spawning` so the
  // user can retry.
  const handleDefaultSpawn = async (issue: GitHubIssue) => {
    if (activeMeshId === null) return;
    setSpawning(issue.number);
    try {
      const defaultProvider = await getDefaultProvider(activeMeshId);
      await createIssueNode(activeMeshId, issue.number, issue.title, defaultProvider);
      setOpenDropdown(null);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn issue agent:', e);
      setSpawning(null);
    }
  };

// Trigger-label toggle handler (issue #979). The Issues Probe shows a
  // green check / neutral slot below the Spawn button for the mesh's
  // configured autopilot trigger label; clicking it adds or removes
  // the label on GitHub. The flow mirrors the optimistic-UI pattern
  // (decision #3 in the locked design):
  //
  //   1. Confirm on remove (it's destructive-ish) — issue #1140 (T2.3)
  //      uses the shared `ConfirmDialog` instead of `window.confirm` so
  //      the prompt matches WorktreeManager's delete confirm. Add is
  //      idempotent on GitHub so it skips the dialog entirely.
  //   2. Flip the issue's labels in `optimisticLabels` so the badge
  //      re-renders without waiting for the IPC.
  //   3. Call `setIssueLabel`. On success, clear pending — the next
  //      `getRepoIssues` refresh will pick up the real state.
  //   4. On error, drop the optimistic override (the source-of-truth
  //      labels show through again) and surface the error message
  //      through the shared toast pipeline (issue #1001 — used to be
  //      inline state below the badge before `addToast` was reachable
  //      from outside App.tsx).
  //
  // The IPC error string is the backend's `Display` impl, which for
  // a 422 → `LabelNotFound` reads "Label `X` doesn't exist on the repo
  // — create it on GitHub first" — exactly the remediation message
  // we want the user to see.

  // Click entry point from the row's badge. Add flows straight into
  // the IPC; remove opens a ConfirmDialog that calls back into
  // `applyLabelToggle` once the user opts in.
  const handleToggleLabel = (issue: GitHubIssue, triggerLabel: string, action: 'add' | 'remove') => {
    if (activeMeshId === null) return;
    if (pendingToggle.has(issue.number)) return;
    if (action === 'remove') {
      setConfirmRemoveFor(issue.number);
      return;
    }
    void applyLabelToggle(issue, triggerLabel, 'add');
  };

  const applyLabelToggle = async (
    issue: GitHubIssue,
    triggerLabel: string,
    action: 'add' | 'remove',
  ) => {
    if (activeMeshId === null) return;

    // Optimistic flip — write the override BEFORE the IPC so the badge
    // re-renders instantly. Source-of-truth `issue.labels` is untouched
    // so a revert is a single state-clear.
    const originalLabels = issue.labels;
    const nextLabels = action === 'add'
      ? (originalLabels.includes(triggerLabel) ? originalLabels : [...originalLabels, triggerLabel])
      : originalLabels.filter((l) => l !== triggerLabel);

    setPendingToggle((prev) => {
      const next = new Set(prev);
      next.add(issue.number);
      return next;
    });
    setOptimisticLabels((prev) => {
      const next = new Map(prev);
      next.set(issue.number, nextLabels);
      return next;
    });

    try {
      await setIssueLabel(activeMeshId, issue.number, triggerLabel, action);
      // Success: the optimistic override stays until the next list
      // refresh. We DO NOT mutate `issues` directly — the source
      // of truth is GitHub, refreshed by `getRepoIssues`.
      // The override is the rendered source until that lands.
    } catch (e) {
      // Revert: drop the override so `issue.labels` shows through again.
      setOptimisticLabels((prev) => {
        if (!prev.has(issue.number)) return prev;
        const next = new Map(prev);
        next.delete(issue.number);
        return next;
      });
      // Issue #1001: surface the failure via the shared toast pipeline
      // (formerly inline error state below the badge). `formatError`
      // unwraps the IPC rejection to the human-readable string the
      // user needs (e.g. "Label `buildmesh:run` doesn't exist on the
      // repo — create it on GitHub first" for a 422, or
      // "GitHub API error (403): ..." for missing triage access).
      // The toast auto-dismisses after TOAST_TTL_MS so a transient
      // failure doesn't linger.
      addToast('GitHub', formatError(e), 'error');
    } finally {
      setPendingToggle((prev) => {
        if (!prev.has(issue.number)) return prev;
        const next = new Set(prev);
        next.delete(issue.number);
        return next;
      });
    }
  };

  // Active mesh's autopilot trigger label — the badge renders whenever
  // this is non-empty. Per the locked design (decision #5), this is
  // independent of whether autopilot itself is enabled: pre-staging
  // labels is useful even when autopilot is off.
  const triggerLabel = useMeshStore((s) => {
    if (activeMeshId === null) return null;
    return s.meshesById.get(activeMeshId)?.autopilot_trigger_label ?? null;
  });

  return (
    <div className="flex flex-col h-full">
      {/* Toolbar mirrors the PRs tab. `SafeLink` falls back to an
          inert <span> when the URL is empty (non-GitHub mesh), so
          the layout stays stable whether or not the URL has resolved. */}
      <ProbeToolbar
        trailing={
          <SafeLink
            url={issuesListUrl}
            ariaLabel="Open this repo's issues list on GitHub"
            title="Open this repo's issues list on GitHub"
            className="text-2xs font-medium text-accent-cyan hover:text-accent-cyan/80 transition-colors"
          >
            View on GitHub ↗
          </SafeLink>
        }
      >
        <RefreshControl
          onRefresh={() => setReloadKey((k) => k + 1)}
          isRefreshing={loading && issues.length > 0}
          ariaLabel="Refresh issues"
        />
      </ProbeToolbar>
      <ProbeTabBody padding="p-3">
        {loading && issues.length === 0 ? (
          // First-load only: refreshes keep the prior list rendered
          // so the user's reading position doesn't reset (mirrors PRs).
          <LoadingState label="Loading issues..." />
        ) : error ? (
          <ErrorState title="Failed to load issues" detail={error} />
        ) : issues.length === 0 ? (
          <EmptyState label="No open issues" />
        ) : (
          <>
            {/* Result count line — issue #1140 (T1.3). One-glance
                sense of scale; rendered above the list when there is
                a list. The EmptyState label above carries the
                zero-count message. */}
            <p className="text-2xs text-text-muted px-1 pb-2">
              {issues.length} open issue{issues.length === 1 ? '' : 's'}
            </p>
            <div className="space-y-1">
            {issues.map(issue => {
              const isExpanded = expanded.isExpanded(issue.number);
              return (
                <ProbeRow
                  key={issue.number}
                  dataAttr="issue"
                  rowKey={issue.number}
                  number={issue.number}
                  title={issue.title}
                  url={issue.url}
                  iconAriaLabel="Open issue on GitHub"
                  isExpanded={isExpanded}
                  onToggle={() => expanded.toggle(issue.number)}
                  body={issue.body}
                  rightSlot={
                    // Canonical `+ ▾` Spawn Menu cluster (ADR-0016 §2). The
                    // sidebar's `NodeCreationForm` renders the same cluster;
                    // rendering it here keeps the issue probe visually
                    // consistent with the rest of the app and saves row
                    // width (single `+` instead of a "Spawn" label). The
                    // `+` auto-spawns via `handleDefaultSpawn`; the `▾`
                    // opens the same `ProviderDropdown` → `GroupedProviderMenu`
                    // ladder the sidebar uses, with `isSpawning` flipping the
                    // `+` to "Spawning…" while this row's stage-2 IPC is in
                    // flight. Wrapped in flex-col so the blocked-by flag can
                    // stack directly under it (issue #481 follow-up).
                    // shrink-0 keeps the right column from being squeezed
                    // by long titles in the left column. `onMouseDown` stop
                    // propagates the click so the row's expand-toggle on
                    // the parent column doesn't fire when the user clicks
                    // the spawn button (and the picker dropdown doesn't get
                    // closed mid-click by the click-outside handler — the
                    // dropdown click-outside is on document mousedown).
                    <div
                      className="flex flex-col items-end shrink-0"
                      onMouseDown={e => e.stopPropagation()}
                    >
                      <SpawnButtonCluster
                        providers={providerList}
                        meshId={issue.number}
                        isOpen={openDropdown === issue.number}
                        onToggleDropdown={() =>
                          setOpenDropdown(openDropdown === issue.number ? null : issue.number)
                        }
                        onSpawnDefault={() => handleDefaultSpawn(issue)}
                        onSelectProvider={(providerId) => handleSpawn(issue, providerId)}
                        disabled={spawning !== null}
                        isSpawning={spawning === issue.number}
                      />
                      {(() => {
                        // Cross-reference the parsed blocked_by list against
                        // the loaded open-issues set. If at least one blocker
                        // is still open, surface the red flag below the spawn
                        // button. This is a warn, not a gate — the Spawn
                        // button stays enabled so a user who's intentionally
                        // unblocking something can still proceed.
                        const stillBlockedBy = issue.blocked_by.filter(n => issuesByNumber.has(n));
                        if (stillBlockedBy.length === 0) return null;
                        const tooltip = buildBlockedByTooltip(stillBlockedBy, issuesByNumber);
                        if (!tooltip) return null;
                        const firstBlocker = stillBlockedBy[0];
                        const firstBlockerUrl = issuesByNumber.get(firstBlocker)?.url ?? '';
                        return (
                          <button
                            data-blocked-by
                            type="button"
                            title={tooltip}
                            aria-label={tooltip}
                            onMouseDown={e => e.stopPropagation()}
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              if (firstBlockerUrl) openUrl(firstBlockerUrl).catch(console.error);
                            }}
                            className="mt-1 inline-flex items-center gap-1 text-status-error hover:text-status-error/80 transition-colors"
                          >
                            <svg
                              width="12"
                              height="12"
                              viewBox="0 0 24 24"
                              fill="none"
                              stroke="currentColor"
                              strokeWidth="2"
                              strokeLinecap="round"
                              strokeLinejoin="round"
                              aria-hidden="true"
                            >
                              <path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z" />
                              <line x1="4" y1="22" x2="4" y2="15" />
                            </svg>
                            <span className="text-xs font-medium leading-none">
                              Blocked by #{firstBlocker}
                            </span>
                          </button>
                        );
                      })()}
                      {(() => {
                        // Trigger-label toggle (issue #979). Sits in the
                        // same right-column flex slot as the blocked-by
                        // flag, so the two stack vertically and the visual
                        // pattern reads as "warnings/affordances under
                        // the Spawn button". Renders whenever the active
                        // mesh has a non-empty `autopilot_trigger_label`
                        // — decision #5: independent of autopilot enabled.
                        if (!triggerLabel) return null;
                        // Source of truth: optimistic override during a
                        // pending toggle, else the loaded issue's labels.
                        // `optimisticLabels` is cleared on revert, so an
                        // error path naturally falls back to `issue.labels`.
                        const effectiveLabels = optimisticLabels.get(issue.number) ?? issue.labels;
                        const present = effectiveLabels.includes(triggerLabel);
                        const isPending = pendingToggle.has(issue.number);
                        return (
                          <div
                            data-trigger-label-row
                            className="mt-1 flex flex-col items-end"
                          >
                            <button
                              data-trigger-label={present ? 'remove' : 'add'}
                              data-trigger-label-name={triggerLabel}
                              data-pending={isPending ? 'true' : 'false'}
                              type="button"
                              title={
                                present
                                  ? `Remove ${triggerLabel} label`
                                  : `Add ${triggerLabel} label`
                              }
                              aria-label={
                                present
                                  ? `Remove ${triggerLabel} label from issue #${issue.number}`
                                  : `Add ${triggerLabel} label to issue #${issue.number}`
                              }
                              disabled={isPending}
                              onMouseDown={e => e.stopPropagation()}
                              onClick={(e) => {
                                e.preventDefault();
                                e.stopPropagation();
                                void handleToggleLabel(
                                  issue,
                                  triggerLabel,
                                  present ? 'remove' : 'add',
                                );
                              }}
                              className={
                                present
                                  ? 'inline-flex items-center gap-1 text-status-success hover:text-status-success/80 transition-colors disabled:opacity-60'
                                  : 'inline-flex items-center gap-1 text-fg-muted hover:text-fg transition-colors disabled:opacity-60'
                              }
                            >
                              <svg
                                width="12"
                                height="12"
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                strokeWidth="2"
                                strokeLinecap="round"
                                strokeLinejoin="round"
                                aria-hidden="true"
                              >
                                {present ? (
                                  <polyline points="20 6 9 17 4 12" />
                                ) : (
                                  <>
                                    <line x1="12" y1="5" x2="12" y2="19" />
                                    <line x1="5" y1="12" x2="19" y2="12" />
                                  </>
                                )}
                              </svg>
                              <span className="text-xs font-medium leading-none">
                                {present ? '✓' : '+'} {triggerLabel}
                              </span>
                            </button>
                          </div>
                        );
                      })()}
                    </div>
                  }
                />
              );
            })}
            </div>
          </>
        )}
      </ProbeTabBody>
      {confirmRemoveFor !== null && (() => {
        const issue = issues.find((i) => i.number === confirmRemoveFor);
        if (!issue || !triggerLabel) return null;
        return (
          <ConfirmDialog
            tone="primary"
            title={`Remove "${triggerLabel}" label from #${issue.number}?`}
            message={`This removes the "${triggerLabel}" label on GitHub for this issue. You can re-apply it later from the same toggle.`}
            confirmLabel="Remove label"
            onCancel={() => setConfirmRemoveFor(null)}
            onConfirm={() => {
              setConfirmRemoveFor(null);
              void applyLabelToggle(issue, triggerLabel, 'remove');
            }}
          />
        );
      })()}
    </div>
  );
}
