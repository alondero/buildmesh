/**
 * The flat two-pane row, operable by keyboard (issue #2424).
 *
 * `AgentNodeView` renders `ResizablePanes` when the scope holds one or
 * two nodes — the row of terminals that sits under the sidebar. Its
 * handle answered the mouse only; it is now the shared `Separator`
 * primitive, so it announces itself, states its range and moves with the
 * arrow keys — inside the same minimum pane size the drag enforces.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { AgentNodeView } from '../../src/components/AgentNodeView/AgentNodeView';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { terminalManager } from '../../src/components/Terminal/Terminal';
import { seedAgentNodes } from './helpers/seedAgentNodes';

// The node card pulls in the terminal tree; the row's own contract is
// the separator, so the card is stubbed to a marker element.
vi.mock('../../src/components/AgentNodeView/NodeCard', () => ({
  NodeCard: ({ nodeId }: { nodeId: number }) => <div data-testid="grid-node" data-node-id={nodeId} />,
}));

vi.mock('../../src/hooks/useProviderList', () => ({
  useProviderList: () => [{
    id: 'claude',
    label: 'Claude Code',
    icon: '',
    harness_id: 'claude',
    provider_id: 'claude',
    is_proxied: false,
    group_key: 'claude',
    resumable: true,
  }],
  useProviderReadiness: () => ({ status: 'loaded', error: null }),
  retryProviderList: () => undefined,
  __resetSharedProviderListForTests: () => undefined,
}));

function makeNode(id: number): AgentNode {
  return {
    id,
    mesh_id: 1,
    name: `agent-${id}`,
    path: '/repo',
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status: 'running',
    cli_session_id: null,
    worktree_name: null,
    use_worktree: false,
    is_pinned: false,
    source_issue: null,
    source_pr: null,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    position: id - 1,
    created_at: '2026-01-01T00:00:00Z',
  } as AgentNode;
}

const NODES: AgentNode[] = [makeNode(1), makeNode(2)];

/** The container width the row falls back to before the first measurement. */
const CONTAINER_WIDTH = 1000;

function separator(): HTMLElement {
  return screen.getByRole('separator', { name: 'Resize pane 1 of 2' });
}

/** The style the first pane carries — the layout model's share. */
function firstPaneWidth(): string {
  const pane = screen.getAllByTestId('grid-node')[0].parentElement as HTMLElement;
  return pane.style.width;
}

let fitAll: ReturnType<typeof vi.spyOn>;
let clientWidth: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  fitAll = vi.spyOn(terminalManager, 'fitAll').mockImplementation(() => {});
  // jsdom reports a zero width for elements; the drag path measures
  // the row's own box, so it needs a real number here.
  clientWidth = vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(1008);
  seedAgentNodes(NODES, NODES[0].id);
  useMeshStore.setState({ selectedMeshId: null });
  useUIStore.setState({
    viewMode: 'all',
    lastNonSingleMode: 'all',
    gridSearchQuery: '',
    gridProviderFilter: null,
    gridStatusFilter: null,
    gridSortBy: 'custom',
    gridSortDirection: 'asc',
    probeOpen: false,
    activeDiffFile: null,
  });
});

afterEach(() => {
  fitAll.mockRestore();
  clientWidth.mockRestore();
  cleanup();
});

describe('the two-pane row separator (issue #2424)', () => {
  it('is a named, focusable separator that states its range', () => {
    render(<AgentNodeView />);

    const handle = separator();
    expect(handle.getAttribute('aria-orientation')).toBe('vertical');
    expect(handle.getAttribute('tabindex')).toBe('0');
    expect(handle.getAttribute('aria-valuenow')).toBe('500');
    // The minimum pane size (300px) plus its share of the handles.
    expect(Number(handle.getAttribute('aria-valuemin'))).toBeGreaterThanOrEqual(300);
    expect(Number(handle.getAttribute('aria-valuemax'))).toBeLessThanOrEqual(CONTAINER_WIDTH);
  });

  it('resizes with the arrow keys — right grows the pane in front of the handle', () => {
    render(<AgentNodeView />);

    fireEvent.keyDown(separator(), { key: 'ArrowRight' });

    expect(separator().getAttribute('aria-valuenow')).toBe('508');
    expect(firstPaneWidth()).toContain('50.8%');
  });

  it('shrinks with ArrowLeft', () => {
    render(<AgentNodeView />);

    fireEvent.keyDown(separator(), { key: 'ArrowLeft' });

    expect(separator().getAttribute('aria-valuenow')).toBe('492');
    expect(firstPaneWidth()).toContain('49.2%');
  });

  it('keeps the minimum pane size at the floor', () => {
    render(<AgentNodeView />);

    fireEvent.keyDown(separator(), { key: 'End' });

    // 302.5px of a 1000px container is 30.25%.
    expect(separator().getAttribute('aria-valuenow')).toBe('303');
    expect(firstPaneWidth()).toContain('30.25%');
  });

  it('refits the terminals when a keyboard resize completes', () => {
    render(<AgentNodeView />);
    const callsBefore = fitAll.mock.calls.length;

    fireEvent.keyDown(separator(), { key: 'ArrowRight' });

    expect(fitAll.mock.calls.length).toBeGreaterThan(callsBefore);
  });

  it('still drags with the pointer after a keyboard resize', () => {
    render(<AgentNodeView />);
    const handle = separator();

    fireEvent.keyDown(handle, { key: 'ArrowRight' });
    expect(handle.getAttribute('aria-valuenow')).toBe('508');

    fireEvent(handle, new MouseEvent('mousedown', { bubbles: true, cancelable: true, clientX: 100 }));
    fireEvent(document, new MouseEvent('mousemove', { bubbles: true, clientX: 140 }));
    fireEvent(document, new MouseEvent('mouseup', { bubbles: true }));

    // The drag takes over from the keyboard's value: +40px on top of 508.
    expect(handle.getAttribute('aria-valuenow')).toBe('548');
  });
});
