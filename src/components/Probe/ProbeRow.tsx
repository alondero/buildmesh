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
 *      swaps between a clamped 2-line plain-text preview and a
 *      scrollable markdown container (GitHub bodies ARE markdown,
 *      so the expanded view renders via `<MarkdownText>`; the
 *      collapsed preview stays raw text because a 2-line clamp of
 *      block-level markdown output can't hold its height and
 *      rendering every collapsed row would multiply the parse
 *      cost by the list length).
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
 *   - `body` renders below BOTH columns, spanning the full row width —
 *     the collapsed 2-line preview and the expanded `max-h-48` reading
 *     panel alike. Long issue/PR text is primary reading material, so it
 *     is not squeezed into the column left of the action buttons. The
 *     collapsed preview is itself clickable (same `onToggle` as the
 *     title column); the expanded panel is a plain, inert reading region
 *     — wrapping scrollable text in a second `role="button"` would break
 *     text selection, trap Space-bar scrolling, and violate the
 *     button-content ARIA rule (buttons must not wrap interactive /
 *     scrollable document content), so collapse happens only from the
 *     title column.
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

import type { ReactNode, Ref } from 'react';
import { SafeLink } from '../shared/SafeLink';
import { MarkdownText } from '../shared/MarkdownText';

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
   * collapsed preview, no expanded panel). Strings of any
   * length render below the title + action columns at full row
   * width — collapsed = a clickable 2-line raw-text preview
   * (`onToggle`), expanded = a plain inert reading panel that
   * renders the body as markdown via `<MarkdownText>`
   * (issue/PR bodies are GitHub markdown) up to `max-h-48`;
   * collapse happens from the title column.
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
  /**
   * Optional metadata rendered as quiet badges directly under the
   * title line (label chips, branch refs, state flags). Rendered
   * identically in collapsed and expanded states so a busy list
   * keeps its vertical rhythm — the badges never disappear when
   * a row opens. Chips use the app-wide 10px `text-2xs` token
   * with a 1px hairline border (`border-border-subtle`) — see
   * `probe-ui-checklist.md` §2.
   */
  metaSlot?: ReactNode;
  /**
   * Optional left-edge status stripe. `'blocked'` paints a 3px
   * `status-warning` vertical bar; `'error'` paints `status-error`.
   * The default renders a transparent stripe of the same width so
   * the row's content never shifts horizontally when a flag appears
   * or disappears on a live refresh. Opaque to the caller.
   */
  status?: 'default' | 'blocked' | 'error';
  /** Ref for the interactive row body used by cross-surface navigation. */
  focusRef?: Ref<HTMLDivElement>;
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
  metaSlot,
  status = 'default',
  focusRef,
}: ProbeRowProps) {
  const hasBody = body !== null && body !== undefined && body !== '';

  const stripeClass =
    status === 'blocked'
      ? 'bg-status-warning'
      : status === 'error'
        ? 'bg-status-error'
        : 'bg-transparent';

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
      className="relative flex flex-col gap-1 rounded-md transition-colors hover:bg-bg-card focus-within:bg-bg-card"
    >
      {/* Left-edge status stripe. Always rendered so the row never
          reflows horizontally when a flag appears/disappears on
          live refresh — the stripe is transparent in the default
          state. `rounded-l-md` clips it to the row's corner. */}
      <span
        aria-hidden="true"
        className={`absolute left-0 top-0 bottom-0 w-[3px] rounded-l-md ${stripeClass}`}
      />
      <div className="flex items-start gap-2 px-2.5 py-2 pl-3">
        {/* Left column — clickable to expand/collapse. The title
            `<a>` and the ↗ icon `<a>` (both via `<SafeLink>`) live
            here, each with stopPropagation so the row handler
            doesn't fire when the user navigates to GitHub. The body
            moved OUT of this column (see below) so it can span the
            full row width. */}
        <div
          ref={focusRef}
          role="button"
          tabIndex={0}
          aria-expanded={hasBody ? isExpanded : undefined}
          aria-disabled={!hasBody || undefined}
          className={`flex-1 min-w-0 rounded-sm focus-visible:outline-none ${
            hasBody
              ? 'cursor-pointer focus-visible:ring-1 focus-visible:ring-accent-cyan'
              : 'cursor-default'
          }`}
          onClick={hasBody ? onToggle : undefined}
          onKeyDown={(e) => {
            if (!hasBody) return;
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              onToggle();
            }
          }}
        >
          <div className="flex items-center gap-1.5 min-w-0">
            {/* Chevron only renders when there IS a body to expand —
                an empty-body row with a chevron implies expandability
                that doesn't exist (affordance lie). */}
            {hasBody && (
              <span
                aria-hidden
                className={
                  'text-text-muted text-2xs w-3 text-center shrink-0 transition-transform duration-150 ' +
                  (isExpanded ? 'rotate-90' : '')
                }
              >
                ▸
              </span>
            )}
            <span className="text-2xs text-accent-cyan font-mono font-medium tabular-nums">
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
              className="text-xs text-text-primary font-medium hover:text-accent-cyan ml-0.5 truncate min-w-0 flex-1 transition-colors"
              title="Open on GitHub"
            >
              {title}
            </SafeLink>
            {url !== '' && (
              <SafeLink
                url={url}
                ariaLabel={iconAriaLabel}
                className="text-text-muted hover:text-accent-cyan transition-colors text-xs shrink-0 leading-none"
                title="Open on GitHub"
              >
                ↗
              </SafeLink>
            )}
          </div>
          {/* Metadata chips — label badges, branch refs, state flags.
              Rendered identically collapsed/expanded so vertical
              rhythm never jumps. */}
          {metaSlot && (
            <div className="flex flex-wrap items-center gap-1 mt-1.5 min-w-0">
              {metaSlot}
            </div>
          )}
        </div>
        {rightSlot}
      </div>
      {/* Body — spans the FULL row width below the title + action
          columns, so long issue/PR text isn't squeezed into the column
          left of the buttons. Only the COLLAPSED preview is a second
          disclosure control (mirrors the title column's handler). The
          EXPANDED panel is plain, inert text — deliberately NOT
          `role="button"`: wrapping a scrollable reading region in a
          button would (1) collapse the row on click-release after every
          text selection, (2) trap Space-bar scrolling (`preventDefault`
          on the wrapper would swallow the panel's own scroll), and
          (3) violate the button-content rule by embedding `overflow-y`
          document content in an ARIA button. Collapse happens from the
          title column (`aria-expanded` there stays the disclosure's
          single control). */}
      {hasBody &&
        (isExpanded ? (
          <div
            data-issue-body-expanded={dataAttr === 'issue' ? true : undefined}
            data-pr-body-expanded={dataAttr === 'pr' ? true : undefined}
            className="px-2.5 pb-2 pl-3"
          >
            <div className="max-h-48 overflow-y-auto overflow-x-hidden text-2xs leading-relaxed text-text-secondary break-words rounded-md border border-border-subtle bg-bg-input px-2 py-1.5">
              {/* Rendered markdown (was raw `whitespace-pre-wrap` text —
                  GitHub bodies are markdown, not plain text). Markdown
                  supplies its own block layout, so the panel drops
                  `whitespace-pre-wrap`; `overflow-x-hidden` keeps a wide
                  child from scrolling the dock sideways (checklist §1)
                  and `break-words` wraps unbounded prose (checklist §2).
                  The panel stays inert — SafeLink's stopPropagation plus
                  the absent wrapper onClick mean a link click can't
                  collapse the row. */}
              <MarkdownText source={body} />
            </div>
          </div>
        ) : (
          <div
            role="button"
            tabIndex={0}
            aria-expanded={false}
            aria-label={`Expand ${dataAttr === 'pr' ? 'pull request' : 'issue'} #${rowKey} description`}
            className="px-2.5 pb-2 pl-3 rounded-sm cursor-pointer focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent-cyan"
            onClick={onToggle}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                onToggle();
              }
            }}
          >
            <p className="text-2xs text-text-muted line-clamp-2 leading-relaxed">
              {body}
            </p>
          </div>
        ))}
      {belowSlot && (
        <div className="px-2.5 pb-1.5 pl-3">{belowSlot}</div>
      )}
    </div>
  );
}
