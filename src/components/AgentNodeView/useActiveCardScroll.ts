import { useLayoutEffect, type RefObject } from 'react';

/** Follow keyboard activity selection, including a member of a grouped card. */
export function useActiveCardScroll(container: RefObject<HTMLElement | null>, activeNodeId: number | null): void {
  useLayoutEffect(() => {
    const member = container.current?.querySelector<HTMLElement>(`[data-node-id="${activeNodeId}"]`);
    const card = member?.closest<HTMLElement>('[data-node-card-id]');
    card?.scrollIntoView?.({ block: 'nearest', inline: 'nearest' });
  }, [container, activeNodeId]);
}
