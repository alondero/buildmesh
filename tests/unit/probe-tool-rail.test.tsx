import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore, type ProbeTab } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { pushProbeWorkingSet, PROBE_WORKING_SET_CAP } from '../../src/lib/probeWorkingSet';
import { PROBE_PANEL_STORAGE_KEY } from '../../src/components/Probe/useProbeResize';

/**
 * ADR-0032 — the Probe tool rail: a working-set tab strip (MRU, capped)
 * inside the open panel plus the grouped "All tools" menu. The pure MRU
 * reducer is tested directly; the component is tested through the full
 * ProbePanel so the tablist/tabpanel wiring is exercised for real.
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

describe('pushProbeWorkingSet (ADR-0032 MRU reducer)', () => {
  it('adds a new destination at the front', () => {
    expect(pushProbeWorkingSet([], 'files')).toEqual(['files']);
    expect(pushProbeWorkingSet(['files'], 'issues')).toEqual(['issues', 'files']);
  });

  it('re-activating an existing entry reorders without growing the set', () => {
    expect(pushProbeWorkingSet(['issues', 'files', 'review'], 'files')).toEqual([
      'files',
      'issues',
      'review',
    ]);
  });

  it('evicts the oldest entry beyond the cap and never exceeds it', () => {
    let set: ReturnType<typeof pushProbeWorkingSet> = [];
    for (const tab of ['files', 'review', 'issues', 'pulls', 'usage'] as const) {
      set = pushProbeWorkingSet(set, tab);
    }
    expect(set).toHaveLength(PROBE_WORKING_SET_CAP);
    // 'files' (visited first) was evicted; the four most recent remain.
    expect(set).toEqual(['usage', 'pulls', 'issues', 'review']);
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
      probeMru: [],
      activeDiffFile: null,
      probeContextPins: {},
    });
    vi.mocked(invoke).mockImplementation(() => Promise.resolve({}));
  });

  it('renders only while the panel is open — closed still means GONE (ADR-0030 discipline)', () => {
    render(<ProbePanel />);
    expect(screen.queryByTestId('probe-tool-rail')).toBeNull();
  });

  it('shows a tab per visited destination in MRU order with the active one selected', () => {
    openPanel('files');
    act(() => {
      useUIStore.getState().openProbeTab('issues');
    });

    const tabs = screen.getByRole('tablist', { name: 'Probe destinations' })
      .querySelectorAll('[role="tab"]');
    expect([...tabs].map((t) => t.getAttribute('data-testid'))).toEqual([
      'probe-rail-tab-issues',
      'probe-rail-tab-files',
    ]);
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

  it('caps the working set at four tabs, evicting the oldest visit', () => {
    openPanel('files');
    for (const tab of ['review', 'issues', 'pulls', 'usage'] as const) {
      act(() => {
        useUIStore.getState().openProbeTab(tab);
      });
    }

    expect(screen.getByRole('tablist', { name: 'Probe destinations' })
      .querySelectorAll('[role="tab"]')).toHaveLength(PROBE_WORKING_SET_CAP);
    expect(useUIStore.getState().probeMru).toEqual(['usage', 'pulls', 'issues', 'review']);
    expect(screen.queryByRole('tab', { name: 'Project Files' })).toBeNull();
  });

  it('moves selection with Arrow keys, activation reordering the MRU', () => {
    openPanel('files');
    act(() => {
      useUIStore.getState().openProbeTab('issues');
    });
    // Working set is MRU-ordered: [issues, files], active tab at the front.
    // Because every activation moves its tab to the front, ArrowRight from
    // a just-activated tab always reaches the *other* entry (it ping-pongs
    // on a two-tab set — exactly the alternation the rail exists for),
    // while ArrowLeft/Home clamp at the front.
    const tablist = screen.getByRole('tablist', { name: 'Probe destinations' });

    fireEvent.keyDown(tablist, { key: 'ArrowRight' });
    expect(useUIStore.getState().probeTab).toBe('files');

    fireEvent.keyDown(tablist, { key: 'ArrowRight' });
    expect(useUIStore.getState().probeTab).toBe('issues');

    // Clamped: 'issues' sits at the front after its activation.
    fireEvent.keyDown(tablist, { key: 'ArrowLeft' });
    expect(useUIStore.getState().probeTab).toBe('issues');

    fireEvent.keyDown(tablist, { key: 'End' });
    expect(useUIStore.getState().probeTab).toBe('files');

    // Clamped: 'files' sits at the front after End's activation.
    fireEvent.keyDown(tablist, { key: 'Home' });
    expect(useUIStore.getState().probeTab).toBe('files');
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

  it('selecting a cold destination from the menu switches to it and adds it to the working set', () => {
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    fireEvent.click(screen.getByTestId('probe-tool-menu-pulls'));

    expect(useUIStore.getState().probeTab).toBe('pulls');
    expect(useUIStore.getState().probeMru).toEqual(['pulls', 'files']);
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

  it('moves menu focus with Arrow keys without switching destination (manual activation)', () => {
    openPanel('files');

    fireEvent.click(screen.getByTestId('probe-rail-all-tools'));
    // Code group grid: files (active, focused) → ArrowRight → review.
    fireEvent.keyDown(screen.getByRole('menu', { name: 'All tools' }), { key: 'ArrowRight' });

    expect(screen.getByTestId('probe-tool-menu-review').getAttribute('tabindex')).toBe('0');
    expect(screen.getByTestId('probe-tool-menu-files').getAttribute('tabindex')).toBe('-1');
    expect(useUIStore.getState().probeTab).toBe('files');
  });

  it('collapses tab labels to icons at the narrow-width breakpoint', () => {
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
  });

  it('keeps visible tab labels at the default width', () => {
    openPanel('files');
    act(() => {
      useUIStore.getState().openProbeTab('issues');
    });

    const tab = screen.getByRole('tab', { name: 'GitHub Issues' });
    expect(tab.textContent).toContain('GitHub Issues');
    expect(tab.getAttribute('aria-label')).toBeNull();
  });
});
