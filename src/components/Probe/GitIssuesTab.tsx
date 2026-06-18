/**
 * GitIssuesTab — the Probe Panel's 🐙 tab body (issue #378).
 *
 * Thin wrapper port of the legacy `GitHubIssuesModal` (issue #113). The
 * dock supplies the header, mesh-name subheading, and close button, so
 * this component drops the modal's backdrop / header / Escape handler
 * and renders the same list + split-spawn button in the probe's 360px
 * body.
 *
 * Two-stage spawn (issue #302)
 * ----------------------------
 * The primary "Spawn" button uses the mesh's resolved default provider
 * (explicit > per-mesh > app-wide > "anthropic" fallback, enforced
 * server-side by `resolve_default_provider`). The `▾` half of the split
 * button opens a provider picker that bypasses the default. Both call
 * sites go through the same two-stage flow:
 *
 *   1. `create_issue_node` — fast DB-only IPC (~20ms) returns a `pending`
 *      node + the prefill to hand off to stage 2.
 *   2. `start_node_background` — fire-and-forget; the slow work (git
 *      fetch, worktree create, PTY spawn) runs in the background, and
 *      `node-spawn-completed` / `node-spawn-failed` store listeners flip
 *      the node to `running` / `error`.
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

import { useState, useEffect, useMemo } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import {
  getRepoIssues,
  createIssueNode,
  startNodeBackground,
  listProviders,
  type GitHubIssue,
} from '../../lib/tauri';
import { useMeshStore } from '../../stores/meshStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { ProviderDropdown, colorClassForProvider, type ProviderEntry } from '../Sidebar/ProviderDropdown';

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
  const { activeMeshId } = useProbeContext();
  // `getDefaultProvider` is mesh-scoped — the only call that needs the
  // meshId directly, since it resolves the per-mesh > app-wide > default
  // precedence chain server-side.
  const getDefaultProvider = useMeshStore((s) => s.getDefaultProvider);

  const [issues, setIssues] = useState<GitHubIssue[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [spawning, setSpawning] = useState<number | null>(null);
  // Only one dropdown open at a time, keyed by issue number — mirrors the
  // SessionBrowserModal pattern so the click-outside handling stays simple.
  const [openDropdown, setOpenDropdown] = useState<number | null>(null);
  // The provider list is fetched once at mount and reused for the lifetime
  // of the tab. Re-fetching on each open would be wasteful, and the list is
  // stable for the duration of a session (adding a new provider requires
  // an app restart).
  const [providerList, setProviderList] = useState<ProviderEntry[]>([]);
  // Per-row expand state for the issue body. Set keyed by issue number
  // (not a single boolean) so cross-referencing two long issues stays
  // possible — the dock is 360px wide, but a user can scroll it freely.
  // Cleared on mesh change in the load effect below.
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const toggleExpanded = (n: number) => setExpanded(prev => {
    const next = new Set(prev);
    if (next.has(n)) next.delete(n); else next.add(n);
    return next;
  });

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
        setExpanded(new Set());
      } catch (e) {
        if (signal.aborted) return;
        console.error('Failed to load issues:', e);
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!signal.aborted) setLoading(false);
      }
    };
    setLoading(true);
    setError(null);
    load();
  }, [activeMeshId]);

  // Fetch the provider list once at mount. Platform filtering (e.g.
  // macOS-only Anthropic) is enforced server-side via
  // AgentProvider::available_on(). Re-fetching on every render would
  // be wasteful, and the list is stable for the lifetime of a session.
  useAsyncEffect((signal) => {
    listProviders()
      .then(backendProviders => {
        if (signal.aborted) return;
        setProviderList(
          backendProviders.map(p => ({ id: p.id, label: p.label, color: colorClassForProvider(p.id) })),
        );
      })
      .catch(err => console.error('listProviders failed:', err));
  }, []);

  // Close the provider dropdown when clicking outside of it. The dropdown
  // container carries a `data-dropdown-for` attribute set to the issue number.
  useEffect(() => {
    if (openDropdown === null) return;
    const handleClick = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (!target.closest(`[data-dropdown-for="${openDropdown}"]`)) {
        setOpenDropdown(null);
      }
    };
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [openDropdown]);

  // Two-stage spawn: stage-1 (`create_issue_node`) is a fast DB-only
  // IPC (~20ms) that returns a `pending` node + the prefill to hand
  // off to stage-2. The dock stays open after a successful spawn so
  // the user can fire off another issue without re-opening the
  // context-menu → "Open Issues" route — the legacy modal's
  // `onClose()` parity was a vestige from the one-shot dialog and
  // doesn't fit a persistent dock. Stage-2 (`startNodeBackground`)
  // is fire-and-forget — the slow work (git fetch, worktree create,
  // PTY spawn) runs in the background, and the node flips to
  // 'running' (or 'error' on failure) via the `node-spawn-completed`
  // / `node-spawn-failed` store listeners. The user dismisses the
  // dock with the activity-bar toggle (or by switching to a non-issues
  // tab) when they're done.
  const handleSpawn = async (issue: GitHubIssue, providerId: string) => {
    if (activeMeshId === null) return;
    setSpawning(issue.number);
    try {
      const draft = await createIssueNode(activeMeshId, issue.number, issue.title, providerId);
      setOpenDropdown(null);
      // Dispatch stage-2 BEFORE clearing the busy state so the
      // fire-and-forget IPC is on the wire before the same-row button
      // re-enables for another click. We deliberately do NOT toggle
      // the probe — the dock stays open so the user can spawn more.
      startNodeBackground(draft.id, draft.prefill);
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
      const draft = await createIssueNode(activeMeshId, issue.number, issue.title, defaultProvider);
      setOpenDropdown(null);
      // Same ordering as `handleSpawn`: stage-2 IPC first, then clear
      // the busy state. Dock stays open (see handleSpawn).
      startNodeBackground(draft.id, draft.prefill);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn issue agent:', e);
      setSpawning(null);
    }
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex-1 overflow-y-auto p-2">
        {loading ? (
          <div className="flex flex-col items-center justify-center py-8 gap-3">
            <div className="animate-spin w-5 h-5 border border-accent-cyan border-t-transparent rounded-full" />
            <span className="text-xs text-text-muted">Loading issues...</span>
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-red-400 mb-2">
              <circle cx="12" cy="12" r="10"/>
              <line x1="15" y1="9" x2="9" y2="15"/>
              <line x1="9" y1="9" x2="15" y2="15"/>
            </svg>
            <span className="text-xs text-red-400">Failed to load issues</span>
            <span className="text-[10px] text-text-muted mt-1 max-w-[280px] text-center">{error}</span>
          </div>
        ) : issues.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-text-muted mb-2">
              <circle cx="12" cy="12" r="10"/>
              <line x1="12" y1="8" x2="12" y2="12"/>
              <line x1="12" y1="16" x2="12.01" y2="16"/>
            </svg>
            <span className="text-xs text-text-muted">No open issues</span>
          </div>
        ) : (
          <div className="space-y-1">
            {issues.map(issue => {
              const isExpanded = expanded.has(issue.number);
              return (
                <div
                  key={issue.number}
                  data-issue-row={issue.number}
                  className="flex items-start gap-2 px-2 py-2 rounded hover:bg-bg-card transition-colors"
                >
                  {/* Left content area — clickable to expand/collapse the
                      body. Title is wrapped in an <a> with stopPropagation
                      so a title click navigates to GitHub without flipping
                      expand. The Spawn button area below is its own
                      stopPropagation island and never bubbles here. */}
                  <div
                    className="flex-1 min-w-0 cursor-pointer"
                    onClick={() => toggleExpanded(issue.number)}
                  >
                    <div className="flex items-center gap-1">
                      <span
                        aria-hidden
                        className={
                          'text-text-muted text-[10px] w-3 text-center shrink-0 transition-transform ' +
                          (isExpanded ? 'rotate-90' : '')
                        }
                      >
                        ▸
                      </span>
                      <span className="text-xs text-accent-cyan font-mono">#{issue.number}</span>
                      {issue.url ? (
                        // The href/target/rel pair keeps the link right-clickable
                        // ("Open in browser"), keyboard-activatable, and screen-reader
                        // friendly. The onClick routes through `openUrl` because the
                        // Tauri 2 WebView is not a browser — `target="_blank"` is
                        // silently dropped without `core:webview:allow-create-webview-window`
                        // (which we don't grant). `e.preventDefault()` stops the dead
                        // default action; `e.stopPropagation()` keeps the row's
                        // expand-toggle out of the way. Mirrors the PR-chip pattern
                        // in `GridNodeHeader.tsx:145` and the web-links addon in
                        // `TerminalRegistry.ts:249`.
                        <a
                          href={issue.url}
                          target="_blank"
                          rel="noopener noreferrer"
                          onClick={(e) => {
                            e.preventDefault();
                            e.stopPropagation();
                            openUrl(issue.url).catch(console.error);
                          }}
                          className="text-sm text-text-primary hover:underline ml-1 truncate"
                          title="Open on GitHub"
                        >
                          {issue.title}
                        </a>
                      ) : (
                        // Upstream html_url is #[serde(default)] on the
                        // Rust side, so a partial GitHub response yields
                        // an empty url — render the title as plain text
                        // to avoid a self-navigating <a href="">.
                        <span className="text-sm text-text-primary ml-1 truncate">
                          {issue.title}
                        </span>
                      )}
                      {issue.url && (
                        // See title <a> above — Tauri 2 needs openUrl routing.
                        <a
                          href={issue.url}
                          target="_blank"
                          rel="noopener noreferrer"
                          onClick={(e) => {
                            e.preventDefault();
                            e.stopPropagation();
                            openUrl(issue.url).catch(console.error);
                          }}
                          aria-label="Open issue on GitHub"
                          className="text-text-muted hover:text-accent-cyan transition-colors text-[11px] shrink-0"
                          title="Open on GitHub"
                        >
                          ↗
                        </a>
                      )}
                    </div>
                    {issue.body && (
                      isExpanded ? (
                        <div
                          data-issue-body-expanded
                          className="mt-1 max-h-48 overflow-y-auto text-[10px] text-text-muted whitespace-pre-wrap break-words"
                        >
                          {issue.body}
                        </div>
                      ) : (
                        <p className="text-[10px] text-text-muted mt-1 line-clamp-2">{issue.body}</p>
                      )
                    )}
                  </div>

                  {/* Split spawn button — primary uses default provider, ▾ opens picker.
                      Wrapped in flex-col so the blocked-by flag can stack directly
                      under it (issue #481 follow-up). shrink-0 keeps the right
                      column from being squeezed by long titles in the left column. */}
                  <div className="flex flex-col items-end shrink-0" onMouseDown={e => e.stopPropagation()}>
                    <div className="relative flex">
                      <button
                        onClick={() => handleDefaultSpawn(issue)}
                        disabled={spawning !== null}
                        className="px-2.5 py-1 text-xs font-medium rounded-l bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                      >
                        {spawning === issue.number ? 'Spawning...' : 'Spawn'}
                      </button>
                      <button
                        onClick={() => setOpenDropdown(openDropdown === issue.number ? null : issue.number)}
                        disabled={spawning !== null}
                        className="px-1.5 py-1 text-xs font-medium rounded-r border-l border-accent-cyan/20 bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                        title="Choose provider"
                      >
                        ▾
                      </button>
                      {openDropdown === issue.number && (
                        <ProviderDropdown
                          meshId={issue.number}
                          providers={providerList}
                          onSelect={(providerId) => handleSpawn(issue, providerId)}
                        />
                      )}
                    </div>
                    {(() => {
                      // Cross-reference the parsed blocked_by list against the
                      // loaded open-issues set. If at least one blocker is still
                      // open, surface the red flag below the spawn button. This
                      // is a warn, not a gate — the Spawn button stays enabled
                      // so a user who's intentionally unblocking something can
                      // still proceed (matches the user's "indicate what issue(s)
                      // are needed to be done first" wording).
                      const stillBlockedBy = issue.blocked_by.filter(n => issuesByNumber.has(n));
                      if (stillBlockedBy.length === 0) return null;
                      const tooltip = buildBlockedByTooltip(stillBlockedBy, issuesByNumber);
                      if (!tooltip) return null;
                      // Click target = the first blocker in source order. The
                      // blocker is guaranteed to be in `issuesByNumber` (it's the
                      // filter that put it in `stillBlockedBy`), so `url` is
                      // always defined — the `?? ''` is just a type-system appeaser.
                      // `openUrl` routes through the OS (Tauri 2 WebView drops
                      // `target="_blank"` without an explicit capability we don't
                      // grant) — mirrors the title-link pattern above.
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
                          className="mt-1 inline-flex items-center gap-1 text-red-400 hover:text-red-300 transition-colors"
                        >
                          {/* Flag SVG — inline (matches the existing empty/error
                              SVGs above, no Lucide dep needed). 12×12 to fit
                              under the spawn button in the 360px dock. */}
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
                          <span className="text-[10px] font-medium leading-none">
                            Blocked by #{firstBlocker}
                          </span>
                        </button>
                      );
                    })()}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
