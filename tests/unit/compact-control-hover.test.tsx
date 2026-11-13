/**
 * Compact-control hover feedback and minimum hit areas (issue #2774).
 *
 * Four compact desktop controls lost their affordances:
 *
 *   - the title bar's View selector trigger had a hover rule that changed
 *     only its INHERITED text colour, while the visible children (the accent
 *     icon and label, the separator, the scope detail, the chevron) set their
 *     own colours — so the control looked the same on hover;
 *   - the grid's removable filter chips (Search / Provider / Status) had no
 *     hover or pressed rule at all, and their accessible names were the
 *     visible text, which does not say what pressing them does;
 *   - the sidebar's mesh title and Show/Hide agents toggle were 18.85px and
 *     16px high with a single pixel between them (pinned in
 *     `sidebar-mesh-item.test.tsx`);
 *   - the node header's attention chip was 24px high but only ~21.75px wide
 *     (pinned in `grid-node-header.test.tsx`).
 *
 * jsdom applies no CSS, so these assertions pin the CLASS contract that
 * produces the rendered result; the browser script
 * `tests/integration/ui-shot-compact-controls.steps.mjs` measures the real
 * bounds and the real computed hover colours.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';

import { ViewSelector } from '../../src/components/TitleBar/ViewSelector';
import { GridFilterPopover } from '../../src/components/TitleBar/GridFilterPopover';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

const MESH: Mesh = {
  id: 1, name: 'demo-1', path: '/repo/1', branch: 'main', position: 0,
  color: null, layout: 'grid', use_worktree: false, created_at: '2026-01-01',
  github_owner: null, github_repo: null, github_last_synced: null, pre_spawn_pool_size: 0,
};

const NODE: AgentNode = {
  id: 1, mesh_id: 1, name: 'agent-a', path: '/repo/1', branch: 'main',
  env: 'wsl', provider: 'claude', status: 'running', cli_session_id: null,
  worktree_name: null, use_worktree: false, is_pinned: false, source_issue: null,
  source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
  source_pr_pinned_sha: null, signal_health: null, position: 0,
  created_at: '2026-01-01', worktree_path: null,
};

function classesOf(el: Element): string[] {
  return (el.getAttribute('class') ?? '').split(/\s+/).filter(Boolean);
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
    loading: false,
    error: null,
  });
  useAgentNodeStore.setState({ activeNodeId: null });
  seedAgentNodes([NODE]);
  useUIStore.setState({
    viewMode: 'all',
    lastNonSingleMode: 'all',
    gridSearchQuery: '',
    gridProviderFilter: null,
    gridStatusFilter: null,
    gridSortBy: 'custom',
    gridSortDirection: 'asc',
    focusGridSearchRequest: 0,
    openScopePickerRequest: 0,
  });
});

afterEach(() => {
  cleanup();
});

describe('View selector trigger (issue #2774)', () => {
  it('paints the surface hover token on hover instead of the inherited text colour', () => {
    render(<ViewSelector />);
    const trigger = screen.getByTestId('view-selector');
    const classes = classesOf(trigger);

    // The surface hover token is what the rest of the desktop chrome uses
    // for a quiet control's hover and pressed state.
    expect(classes).toContain('hover:bg-bg-card-hover');
    expect(classes).toContain('active:bg-bg-card-hover');
    // The old rule had no effect: the visible children set their own
    // colours, so an inherited colour change did not reach the pixels.
    expect(classes).not.toContain('hover:text-text-primary');
  });

  it('keeps the active View Mode legible while hovering — the children own their colours', () => {
    render(<ViewSelector />);
    const trigger = screen.getByTestId('view-selector');
    // The trigger's only hover rule is the surface token, so there is no
    // inherited colour rule left to lose.
    const hoverRules = classesOf(trigger).filter((c) => c.startsWith('hover:'));
    expect(hoverRules).toEqual(['hover:bg-bg-card-hover']);
    // The visible children set their own colours, which is why an inherited
    // colour change left this trigger looking the same.
    const visibleChildren = Array.from(trigger.children)
      .filter((el) => el.getAttribute('aria-hidden') !== 'true');
    expect(visibleChildren.length).toBeGreaterThan(0);
    for (const child of visibleChildren) {
      const colourClasses = classesOf(child).filter((c) => c.startsWith('text-'));
      expect(
        colourClasses.length,
        `${child.tagName}.${child.className} carries no explicit colour`,
      ).toBeGreaterThan(0);
    }
  });
});

describe('Grid filter removal chips (issue #2774)', () => {
  function openPanel() {
    render(<GridFilterPopover />);
    fireEvent.click(screen.getByRole('button', { name: /Filters and sort:/ }));
    return screen.getByRole('region', { name: 'Filters and sort' });
  }

  it('gives the three removable chips the surface hover token, the pressed state and a 24px target', () => {
    useUIStore.setState({
      gridSearchQuery: 'alpha',
      gridProviderFilter: 'claude',
      gridStatusFilter: 'running',
    });
    openPanel();

    const chips = screen.getAllByRole('button', { name: /^Remove (search|provider|status) filter/ });
    expect(chips).toHaveLength(3);
    for (const chip of chips) {
      const classes = classesOf(chip);
      expect(classes).toContain('hover:bg-bg-card-hover');
      expect(classes).toContain('active:bg-bg-card-hover');
      expect(classes).toContain('min-h-[24px]');
    }
  });

  it('names each chip with the action it performs, and keeps the visible label meaningful', () => {
    useUIStore.setState({
      gridSearchQuery: 'alpha',
      gridProviderFilter: 'claude',
      gridStatusFilter: 'running',
    });
    openPanel();

    expect(screen.getByRole('button', { name: 'Remove search filter: alpha' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Remove provider filter: claude' })).toBeTruthy();
    expect(screen.getByRole('button', { name: /Remove status filter:/ })).toBeTruthy();

    // The visible labels still say which filter they carry.
    const panel = screen.getByRole('region', { name: 'Filters and sort' });
    expect(panel.textContent).toContain('Search: alpha');
    expect(panel.textContent).toContain('Provider: claude');
    expect(panel.textContent).toContain('Status:');
  });

  it('removes only the filter whose chip was pressed', () => {
    useUIStore.setState({
      gridSearchQuery: 'alpha',
      gridProviderFilter: 'claude',
      gridStatusFilter: 'running',
    });
    openPanel();

    fireEvent.click(screen.getByRole('button', { name: 'Remove provider filter: claude' }));

    const state = useUIStore.getState();
    expect(state.gridProviderFilter).toBeNull();
    // The other two filters are untouched — the chip removes one filter and
    // leaves the other two in place.
    expect(state.gridSearchQuery).toBe('alpha');
    expect(state.gridStatusFilter).toBe('running');
  });

  it('removes the search filter on its own too', () => {
    useUIStore.setState({
      gridSearchQuery: 'alpha',
      gridProviderFilter: 'claude',
    });
    openPanel();

    fireEvent.click(screen.getByRole('button', { name: 'Remove search filter: alpha' }));

    const state = useUIStore.getState();
    expect(state.gridSearchQuery).toBe('');
    expect(state.gridProviderFilter).toBe('claude');
  });
});
