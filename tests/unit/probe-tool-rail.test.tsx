import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore, type ProbeTab } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import {
  pushProbeWorkingSet,
  EMPTY_PROBE_WORKING_SET,
  PROBE_WORKING_SET_CAP,
  type ProbeWorkingSet,
} from '../../src/lib/probeWorkingSet';
import { PROBE_PANEL_STORAGE_KEY } from '../../src/components/Probe/useProbeResize';

/**
 * ADR-0032 — the Probe tool rail: a working-set tab strip (capped) inside
 * the open panel plus the grouped "All tools" menu. The pure reducer is
 * tested directly; the component is tested through the full ProbePanel so
 * the tablist/tabpanel wiring is exercised for real.
 *
 * Navigation contract under test (the review-found defect this pins):
 * display order is insertion-ordered and spatially stable — arrows walk
 * real positions, so every working-set entry is reachable in BOTH
 * directions even though activation updates the recency list.
 */

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
  scratchpad: '',
  sandbox: false,
};

const NODE: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo/worktrees/agent-1',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  use_worktree: true,
  position: 0,
  created_at: new Date(0).toISOString(),
};

function openPanel(tab: ProbeTab): void {
  act(() => {
    useUIStore.getState().openProbeTab(tab);
  });
  render(<ProbePanel />);
}

function railTabIds(): string[] {
  return [...screen.getByRole('tablist', { name: 'Probe destinations' })
    .querySelectorAll('[role="tab"]')].map((t) => t.getAttribute('data-testid'));
}

describe('pushProbeWorkingSet (ADR-0032 reducer)', () => {
  it('appends a new destination at the end of the display order', () => {
    const afterFiles = pushProbeWorkingSet(EMPTY_PROBE_WORKING_SET, 'files');
    expect(afterFiles).toEqual({ tabs: ['files'], mru: ['files'] });

    const afterIssues = pushProbeWorkingSet(afterFiles, 'issues');
    expect(afterIssues.tabs).toEqual(['files', 'issues']);
    expect(afterIssues.mru).toEqual(['issues', 'files']);
  });

  it('re-activation updates recency but keeps the display position', () => {
    const start: ProbeWorkingSet = {
      tabs: ['files', 'review', 'issues'],
      mru: ['issues', 'review', 'files'],
    };
    const next = pushProbeWorkingSet(start, 'files');
    // 'files' stays at position 0 — arrows walk stable positions.
    expect(next.tabs).toEqual(['files', 'review', 'issues']);
    expect(next.mru).toEqual(['files', 'issues', 'review']);
  });

  it('evicts the least recently visited beyond the cap and appends the new tab', () => {
    let set: ProbeWorkingSet = EMPTY_PROBE_WORKING_SET;
    for (const tab of ['files', 'review', 'issues', 'usage'] as const) {
      set = pushProbeWorkingSet(set, tab);
    }
    expect(set.tabs).toEqual(['files', 'review', 'issues', 'usage']);
    expect(set.mru).toEqual(['usage', 'issues', 'review', 'files']);

    set = pushProbeWorkingSet(set, 'pulls');
    expect(set.mru).toEqual(['pulls', 'usage', 'issues', 'review']);
    // 'files' (least recently visited) dropped out of display too; 'pulls'
    // appended at the end; the middle entries keep their relative order.
    expect(set.tabs).toEqual(['review', 'issues', 'usage', 'pulls']);
    expect(set.tabs).toHaveLength(PROBE_WORKING_SET_CAP);
  });
});

describe('ProbeToolRail (ADR-0032)', () => {
  beforeEach(() => {
    // The resize hook persists the settled width on mount; a 240px write
    // from the narrow-width case would leak into the next test's mount and
    // silently collapse every tab to icons.
    window.localStorage.removeItem(PROBE_PANEL_STORAGE_KEY);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    seedAgentNodes([NODE], NODE.id);
    useUIStore.setState({
      probeOpen: false,
      probeTab: 'files',
      probeWorkingSet: EMPTY_PROBE_WORKING_SET,
      activeDiffFile: null,
      probeContextPins: {},
    });
    vi.mocked(invoke).mockImplementation(() => Promise.resolve({}));
  });

  it('renders only while the panel is open — closed still means GONE (ADR-0030 discipline)', () => {
    render(<ProbePanel />);
    expect(screen.queryByTestId('probe-tool-rail')).toBeNull();
  });

  it('shows tabs in insertion order with the active one selected', () => {
    openPanel('files');
    act(() => {
      useUIStore.getState().openProbeTab('issues');
    });

    // Display order is insertion order — NOT recency order.
    expect(railTabIds()).toEqual(['probe-rail-tab-files', 'probe-rail-tab-issues']);
    expect(screen.getByRole('tab', { name: 'GitHub Issues' }).getAttribute('aria-selected')).toBe('true');
    expect(screen.getByRole('tab', { name: 'Project Files' }).getAttribute('aria-selected')).toBe('false');
  });

  it('switches destination when a rail tab is clicked', () => {
    openPanel('files');
    act(() => {
      useUIStore.getState().openProbeTab('issues');
    });

    fireEvent.click(screen.getByRole('tab', { name: 'Project Files' }));
    expect(useUIStore.getState().probeTab).toBe('files');
    expect(screen.getByTestId('probe-rail-tab-files').getAttribute('aria-selected')).toBe('true');
  });

  it('caps the working set at four tabs, evicting the least recently visited', () => {
    openPanel('files');
    for (const tab of ['review', 'issues', 'pulls', 'usage'] as const) {
      act(() => {
        useUIStore.getState().openProbeTab(tab);
      });
    }

    expect(railTabIds()).toHaveLength(PROBE_WORKING_SET_CAP);
    expect(useUIStore.getState().probeWorkingSet.mru).toEqual(['usage', 'pulls', 'issues', 'review']);
    expect(useUIStore.getState().probeWorkingSet.tabs).toEqual(['review', 'issues', 'pulls', 'usage']);
    expect(screen.queryByRole('tab', { name: 'Project Files' })).toBeNull();
  });

  it('walks ALL working-set entries with ArrowRight and ArrowLeft (no ping-pong)', () => {
    openPanel('files');
    for (const tab of ['review', 'issues', 'usage'] as const) {
      act(() => {
        useUIStore.getState().openProbeTab(tab);
      });
    }
    // Display order [files, review, issues, usage]; move back to the front
    // entry to start the walk. Activation must not reorder the strip.
    act(() => {
      useUIStore.getState().openProbeTab('files');
    });
    const tablist = screen.getByRole('tablist', { name: 'Probe destinations' });
    const walk = (key: string) => {
      fireEvent.keyDown(tablist, { key });
      return useUIStore.getState().probeTab;
    };

    expect(walk('ArrowRight')).toBe('review');
    expect(walk('ArrowRight')).toBe('issues');
    expect(walk('ArrowRight')).toBe('usage');
    expect(walk('ArrowRight')).toBe('usage'); // clamped at the end

    expect(walk('ArrowLeft')).toBe('issues');
    expect(walk('ArrowLeft')).toBe('review');
    expect(walk('ArrowLeft')).toBe('files');
    expect(walk('ArrowLeft')).toBe('files'); // clamped at the front
  });

  it('keeps tab positions spatially stable across activations', () => {
    openPanel('files');
    act(() => {
      useUIStore.getState().openProbeTab('issues');
    });
    expect(railTabIds()).toEqual(['probe-rail-tab-files', 'probe-rail-tab-issues']);

    act(() => {
      useUIStore.getState().openProbeTab('files');
    });
    // Activating the FIRST tab must not shuffle it (or anything else) —
    // recency drives eviction only, never display position.
    expect(railTabIds()).toEqual(['probe-rail-tab-files', 'probe-rail-tab-issues']);
  });

  it('records a visit when the panel opens via toggleProbe, so the rail is never empty', () => {
    // Review-found defect: a fresh session + toggleProbe() used to open the
    // dock on an empty working set, rendering an empty bar and leaving the
    // body's aria-labelledby pointing at a non-existent tab.
    useUIStore.setState({
      probeOpen: false,
      probeTab: 'files',
      probeWorkingSet: { tabs: ['files'], mru: ['files'] },
    });
    render(<ProbePanel />);

    act(() => {
      useUIStore.getState().toggleProbe();
    });
    expect(railTabIds()).toEqual(['probe-rail-tab-files']);
    // aria-labelledby resolves to a mounted element.
    expect(document.getElementById('probe-rail-tab-files')).not.toBeNull();
  });

  it('opens the grouped tool menu from the ⊞ affordance with every destination', () => {
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));

    expect(screen.getByTestId('probe-rail-all-tools').getAttribute('aria-expanded')).toBe('true');
    expect(screen.getAllByRole('menuitemradio')).toHaveLength(11);
    expect(screen.getByTestId('probe-tool-menu-files').getAttribute('aria-checked')).toBe('true');
    expect(screen.getByText('Code')).toBeTruthy();
    expect(screen.getByText('App-wide')).toBeTruthy();
  });

  it('selecting a cold destination from the menu switches to it and appends it to the working set', () => {
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    fireEvent.click(screen.getByTestId('probe-tool-menu-pulls'));

    expect(useUIStore.getState().probeTab).toBe('pulls');
    expect(useUIStore.getState().probeWorkingSet.tabs).toEqual(['files', 'pulls']);
    expect(useUIStore.getState().probeWorkingSet.mru).toEqual(['pulls', 'files']);
    expect(screen.queryByRole('menu', { name: 'All tools' })).toBeNull();
    // Focus returns to the trigger after menu selection.
    expect(document.activeElement).toBe(screen.getByTestId('probe-rail-all-tools'));
  });

  it('closes the menu on Escape and restores focus to the ⊞ trigger', () => {
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    fireEvent.keyDown(screen.getByRole('menu', { name: 'All tools' }), { key: 'Escape' });

    expect(screen.queryByRole('menu', { name: 'All tools' })).toBeNull();
    expect(screen.getByTestId('probe-rail-all-tools').getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(screen.getByTestId('probe-rail-all-tools'));
  });

  it('closes the menu when a mousedown lands outside it', () => {
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    expect(screen.getByRole('menu', { name: 'All tools' })).toBeTruthy();

    fireEvent.mouseDown(screen.getByTestId('probe-context-pin'));
    expect(screen.queryByRole('menu', { name: 'All tools' })).toBeNull();
  });

  it('closes the menu when focus leaves the rail (Tab away)', () => {
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    expect(screen.getByRole('menu', { name: 'All tools' })).toBeTruthy();

    // Focusout to an element outside the rail — the menu-button contract
    // forbids an orphaned floating menu.
    fireEvent.focusOut(screen.getByTestId('probe-rail-all-tools'), {
      relatedTarget: document.body,
    });
    expect(screen.queryByRole('menu', { name: 'All tools' })).toBeNull();
  });

  it('opens the menu from the trigger with ArrowDown (first tile) and ArrowUp (last tile)', () => {
    openPanel('files');
    const trigger = screen.getByTestId('probe-rail-all-tools');

    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    expect(screen.getByRole('menu', { name: 'All tools' })).toBeTruthy();
    expect(document.activeElement?.id).toBe('probe-rail-menu-files');

    fireEvent.keyDown(screen.getByRole('menu', { name: 'All tools' }), { key: 'Escape' });
    fireEvent.keyDown(trigger, { key: 'ArrowUp' });
    expect(screen.getByRole('menu', { name: 'All tools' })).toBeTruthy();
    expect(document.activeElement?.id).toBe('probe-rail-menu-usage');
  });

  it('moves menu focus with Arrow keys without switching destination (manual activation)', () => {
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    const menu = screen.getByRole('menu', { name: 'All tools' });
    // Wide widths keep the 2-column virtual grid: right moves one column,
    // down crosses group headings two tiles at a time.
    fireEvent.keyDown(menu, { key: 'ArrowRight' });
    // Code group grid: files (active, focused) → ArrowRight → review.
    expect(screen.getByTestId('probe-tool-menu-review').getAttribute('tabindex')).toBe('0');
    expect(useUIStore.getState().probeTab).toBe('files');

    fireEvent.keyDown(menu, { key: 'ArrowDown' });
    expect(screen.getByTestId('probe-tool-menu-properties').getAttribute('tabindex')).toBe('0');
    expect(useUIStore.getState().probeTab).toBe('files');
  });

  it('walks the narrow menu as a 1D list — ArrowDown reaches the adjacent tile, horizontal arrows are inert', () => {
    // Review-found defect: the menu stacks to one column when narrow, but
    // the keyboard model kept the 2-column virtual grid — ArrowDown skipped
    // every other tile and ArrowRight/Left jumped across phantom columns.
    window.localStorage.setItem(PROBE_PANEL_STORAGE_KEY, '240');
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    expect(document.activeElement?.id).toBe('probe-rail-menu-files');
    const menu = screen.getByRole('menu', { name: 'All tools' });

    // Under the old model this landed on 'worktrees' (a full virtual row down).
    fireEvent.keyDown(menu, { key: 'ArrowDown' });
    expect(document.activeElement?.id).toBe('probe-rail-menu-review');

    fireEvent.keyDown(menu, { key: 'ArrowDown' });
    expect(document.activeElement?.id).toBe('probe-rail-menu-worktrees');

    // No second column exists — horizontal arrows do not move focus.
    fireEvent.keyDown(menu, { key: 'ArrowRight' });
    expect(document.activeElement?.id).toBe('probe-rail-menu-worktrees');
    fireEvent.keyDown(menu, { key: 'ArrowLeft' });
    expect(document.activeElement?.id).toBe('probe-rail-menu-worktrees');
    expect(useUIStore.getState().probeTab).toBe('files');

    // ArrowUp walks back and the column wraps at both ends.
    fireEvent.keyDown(menu, { key: 'ArrowUp' });
    expect(document.activeElement?.id).toBe('probe-rail-menu-review');
    fireEvent.keyDown(menu, { key: 'ArrowUp' });
    expect(document.activeElement?.id).toBe('probe-rail-menu-files');
    fireEvent.keyDown(menu, { key: 'ArrowUp' });
    expect(document.activeElement?.id).toBe('probe-rail-menu-usage');
  });

  it('collapses tab labels to icons and stacks the menu in one column at the narrow-width breakpoint', () => {
    // The narrow prop derives from the persisted panel body width; seed the
    // same storage key the resize hook reads so the mount boots narrow.
    window.localStorage.setItem(PROBE_PANEL_STORAGE_KEY, '240');
    openPanel('files');
    act(() => {
      useUIStore.getState().openProbeTab('issues');
    });

    const tab = screen.getByRole('tab', { name: 'GitHub Issues' });
    // Icon-only: the visible label is gone but the accessible name remains
    // via aria-label (probe-ui-checklist.md §4 — icon-only buttons name
    // their object).
    expect(tab.textContent).not.toContain('GitHub Issues');
    expect(tab.getAttribute('aria-label')).toBe('GitHub Issues');

    // The menu must drop to one column — a 2-column grid truncates tile
    // names to noise at the dock's 240px minimum (probe-ui-checklist.md §2).
    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    const menu = screen.getByRole('menu', { name: 'All tools' });
    expect(menu.querySelector('.grid-cols-1')).not.toBeNull();
    expect(menu.querySelector('.grid-cols-2')).toBeNull();
  });

  it('keeps visible tab labels and the two-column menu at the default width', () => {
    openPanel('files');
    act(() => {
      useUIStore.getState().openProbeTab('issues');
    });

    const tab = screen.getByRole('tab', { name: 'GitHub Issues' });
    expect(tab.textContent).toContain('GitHub Issues');
    expect(tab.getAttribute('aria-label')).toBeNull();

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    const menu = screen.getByRole('menu', { name: 'All tools' });
    expect(menu.querySelector('.grid-cols-2')).not.toBeNull();
  });
});
