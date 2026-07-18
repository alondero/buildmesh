/**
 * ProbeTabBody — the standard padding wrapper for every probe tab body
 * (issue #842, predecessor #813). Replaces the per-tab hand-rolled
 * `flex-1 overflow-y-auto p-{2|3|4}` divs that drifted apart over the
 * first half of the probe consolidation. Tabs that need a different
 * rhythm (none today) override via `padding`; the standard `p-3` puts
 * the body in lockstep with the header's `px-3 py-2` so a tab switch
 * no longer visibly resizes the content area.
 */

import type { HTMLAttributes, ReactNode } from 'react';

export type ProbeTabPadding = 'p-2' | 'p-3' | 'p-4';

type DivProps = HTMLAttributes<HTMLDivElement>;

interface ProbeTabBodyProps extends Omit<DivProps, 'children'> {
  /** Body padding. Default `p-3` matches `UsageTab`'s pre-existing choice
   *  and sits visually between the dense `p-2` list tabs and the airy
   *  `p-4` form tabs — pick this unless you have a specific reason. */
  padding?: ProbeTabPadding;
  /** Extra classNames appended after the wrapper styles. Use sparingly —
   *  the intent of this primitive is that `padding` is the only knob. */
  className?: string;
  children: ReactNode;
}

export function ProbeTabBody({
  padding = 'p-3',
  className = '',
  children,
  ...rest
}: ProbeTabBodyProps) {
  return (
    <div
      {...rest}
      className={`flex-1 overflow-y-auto ${padding} ${className}`.trim()}
    >
      {children}
    </div>
  );
}
