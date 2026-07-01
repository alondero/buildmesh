/**
 * `<ProbeRow>` — issue #463.
 *
 * The shared row body for the Probe dock's list tabs. PRs #459 and
 * #461 added the click-to-expand + title hyperlink pattern to
 * `GitIssuesTab` and `GitPullRequestsTab` independently, leaving the
 * two tabs ~80% structurally identical. This component consolidates
 * the duplication into one place.
 *
 * The five contracts it pins (sourced from #459/#461 + the
 * `buildmesh-empty-url-frontend-guard` memory):
 *
 *   1. Click-to-expand. Clicking the row's body text fires
 *      `onToggle` — the chevron rotates 90° and the body region
 *      swaps between a clamped 2-line preview and a scrollable
 *      full-text container.
 *   2. Title link. The title is an `<a>` (via `<SafeLink>`) with
 *      `target="_blank"`, `rel="noopener noreferrer"`, and an
 *      onClick that routes through `openUrl` (Tauri 2 drops
 *      `target="_blank"` without the capability we don't grant).
 *   3. Link doesn't toggle. The link's onClick calls
 *      `stopPropagation` so the row's expand-toggle doesn't fire
 *      on the way to GitHub.
 *   4. Empty-URL fallback. When `url === ''`, the title renders
 *      as a plain `<span>` — no `<a href="">` (the WebView would
 *      self-navigate).
 *   5. Right-click + AT preserved. The `<a href>` is unchanged so
 *      right-click → "Open in browser", ⌘-click, and screen readers
 *      still work.
 *
 * Slot API
 * --------
 * Tab-specific actions are passed in as opaque React nodes:
 *
 *   - `rightSlot`: rendered to the right of the title column, as a
 *     sibling of the clickable column (so clicking the slot's
 *     buttons does NOT trigger the row's onToggle). The issues
 *     tab passes the split spawn button + blocked-by flag; the PR
 *     tab passes the merge + spawn + view-changes triad.
 *   - `belowSlot`: rendered below the title row, inside the outer
 *     `<div data-X-row>`. The PR tab uses this for inline
 *     merge-error / spawn-error rows.
 *
 * The slot wrappers in the *caller* carry the `onMouseDown`
 * stopPropagation for the dropdown click-outside handler —
 * ProbeRow doesn't know about dropdowns.
 *
 * The `dataAttr` prop
 * -------------------
 * Tests + downstream CSS query the row by `data-issue-row` /
 * `data-pr-row` and the body by `data-issue-body-expanded` /
 * `data-pr-body-expanded`. The prefix is supplied per-callsite;
 * ProbeRow does not hard-code which tab it belongs to. This keeps
 * the component reusable for future list tabs (e.g. a worktrees
 * list that wants the same row body without `data-pr-*`).
 */

import type { ReactNode } from 'react';
import { SafeLink } from '../shared/SafeLink';

export interface ProbeRowProps {
  /**
   * Prefix used to build the test/CSS selector attributes. The row
   * becomes `${dataAttr}-row={rowKey}`; the expanded body becomes
   * `${dataAttr}-body-expanded`. Typed as a discriminator union so a
   * typo (`'issus'`) is caught at the call site instead of silently
   * breaking the selectors that downstream tests query.
   */
  dataAttr: 'issue' | 'pr';
  /** Value for the data attribute — the issue / PR number. */
  rowKey: string | number;

  /** `#NN` prefix rendered in cyan font-mono. */
  number: number;
  /** Title text — also the link body. */
  title: string;
  /**
   * External URL. The empty string (`''`) falls back to a `<span>`
   * for the title and hides the ↗ icon — see
   * `buildmesh-empty-url-frontend-guard`.
   */
  url: string;
  /**
   * Accessible name for the ↗ icon. Tab-specific: `Open issue on
   * GitHub` for the issues tab, `Open pull request on GitHub` for
   * the PRs tab.
   */
  iconAriaLabel: string;

  /** Whether the body region is currently expanded. */
  isExpanded: boolean;
  /** Fired when the user clicks the body / chevron / padding. */
  onToggle: () => void;

  /**
   * Body text. Null/empty hides the body region entirely (no
   * clamped paragraph, no scrollable container). Strings of any
   * length render — collapsed = clamped to 2 lines, expanded =
   * scrollable up to `max-h-48`.
   */
  body?: string | null;

  /**
   * Tab-specific actions on the right (split spawn button, merge
   * control, view-changes button, blocked-by flag, etc.). Rendered
   * as a sibling of the clickable column, so clicks inside the
   * slot do not bubble to `onToggle`. Opaque to ProbeRow.
   */
  rightSlot?: ReactNode;
  /**
   * Tab-specific extras below the title row (e.g. inline merge /
   * spawn error messages). Rendered inside the outer
   * `${dataAttr}-row` div, below the inner title row. Opaque to
   * ProbeRow.
   */
  belowSlot?: ReactNode;
}

export function ProbeRow({
  dataAttr,
  rowKey,
  number,
  title,
  url,
  iconAriaLabel,
  isExpanded,
  onToggle,
  body,
  rightSlot,
  belowSlot,
}: ProbeRowProps) {
  const hasBody = body !== null && body !== undefined && body !== '';

  return (
    // The outer wrapper is a `flex flex-col gap-1` even when there's
    // no `belowSlot` — the gap is invisible with a single child
    // (visual parity with the single-row layout the issues tab uses)
    // and shows the spacing when errors stack below. Unifying on the
    // stacked layout lets one component serve both the issues and PR
    // tabs without a layout branch.
    <div
      data-issue-row={dataAttr === 'issue' ? rowKey : undefined}
      data-pr-row={dataAttr === 'pr' ? rowKey : undefined}
      className="flex flex-col gap-1 px-2 py-2 rounded hover:bg-bg-card transition-colors"
    >
      <div className="flex items-start gap-2">
        {/* Left column — clickable to expand/collapse. The title
            `<a>` and the ↗ icon `<a>` (both via `<SafeLink>`) live
            here, each with stopPropagation so the row handler
            doesn't fire when the user navigates to GitHub. */}
        <div
          role="button"
          tabIndex={0}
          aria-expanded={isExpanded}
          className="flex-1 min-w-0 cursor-pointer"
          onClick={onToggle}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              onToggle();
            }
          }}
        >
          <div className="flex items-center gap-1 min-w-0">
            <span
              aria-hidden
              className={
                'text-text-muted text-2xs w-3 text-center shrink-0 transition-transform ' +
                (isExpanded ? 'rotate-90' : '')
              }
            >
              ▸
            </span>
            <span className="text-xs text-accent-cyan font-mono">
              #{number}
            </span>
            {/* Title link. `min-w-0 flex-1` on the link AND `min-w-0`
                on the parent flex are required for the `truncate`
                class to actually take effect — see the
                `flexbox-truncate-trap` memory and the PR-tab
                regression test. Without them a long title wraps
                into the action buttons. */}
            <SafeLink
              url={url}
              className="text-sm text-text-primary hover:underline ml-1 truncate min-w-0 flex-1"
              title="Open on GitHub"
            >
              {title}
            </SafeLink>
            {url !== '' && (
              <SafeLink
                url={url}
                ariaLabel={iconAriaLabel}
                className="text-text-muted hover:text-accent-cyan transition-colors text-xs shrink-0"
                title="Open on GitHub"
              >
                ↗
              </SafeLink>
            )}
          </div>
          {hasBody &&
            (isExpanded ? (
              <div
                data-issue-body-expanded={dataAttr === 'issue' ? true : undefined}
                data-pr-body-expanded={dataAttr === 'pr' ? true : undefined}
                className="mt-1 max-h-48 overflow-y-auto text-2xs text-text-muted whitespace-pre-wrap break-words"
              >
                {body}
              </div>
            ) : (
              <p className="text-2xs text-text-muted mt-1 line-clamp-2">
                {body}
              </p>
            ))}
        </div>
        {rightSlot}
      </div>
      {belowSlot}
    </div>
  );
}