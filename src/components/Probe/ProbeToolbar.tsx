/**
 * ProbeToolbar — the standard action strip that sits between the dock
 * header and a fetch-driven tab's body (Usage, Git Issues, Pull Requests,
 * Archive).
 *
 * Before this primitive each tab hand-rolled the same
 * `px-3 py-2 border-b flex items-center justify-between` row and the
 * details drifted: padding, gaps, which side Refresh sat on, and whether
 * the row could shrink under a long "View on GitHub ↗" link all varied
 * from tab to tab. One component pins the rhythm:
 *
 *   - left slot (`children`): the tab's primary controls — Refresh,
 *     a search input, a result count. It grows (`flex-1 min-w-0`) so a
 *     search field can fill the row and long text truncates instead of
 *     pushing the trailing slot out of the dock.
 *   - right slot (`trailing`): secondary navigation-style actions —
 *     "View on GitHub ↗", the open/closed segmented toggle. `shrink-0`
 *     keeps it visible at the narrowest (240px) panel width.
 *
 * The 37px minimum height matches the pre-convergence rows so tab
 * switches don't visibly resize the strip.
 */

import type { ReactNode } from 'react';

interface ProbeToolbarProps {
  /** Primary controls (Refresh, search, counts). */
  children: ReactNode;
  /** Secondary actions pinned to the right edge. */
  trailing?: ReactNode;
  /** Extra classNames appended after the wrapper styles. */
  className?: string;
}

export function ProbeToolbar({ children, trailing, className = '' }: ProbeToolbarProps) {
  return (
    <div
      className={`flex items-center gap-2 px-3 py-2 border-b border-border-subtle min-h-[37px] ${className}`.trim()}
    >
      <div className="flex items-center gap-2 min-w-0 flex-1">{children}</div>
      {trailing && <div className="flex items-center gap-2 shrink-0">{trailing}</div>}
    </div>
  );
}
