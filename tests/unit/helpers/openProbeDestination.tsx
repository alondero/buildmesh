import { render } from '@testing-library/react';
import { ProbePanel } from '../../../src/components/Probe/ProbePanel';
import { useUIStore, type ProbeTab } from '../../../src/stores/uiStore';

/**
 * Mount the full ProbePanel with the given inspector destination already
 * opened via `openProbeTab` — the on-demand entry point since #1375 removed
 * the always-visible rail. Opening through the store first keeps the mount
 * path identical to the real entry points (palette, title bar, contextual
 * menus) and lets tests use synchronous `getBy*` lookups against the header
 * instead of awaiting a click-driven mount.
 */
export function openProbeDestination(tab: ProbeTab): void {
  useUIStore.getState().openProbeTab(tab);
  render(<ProbePanel />);
}
