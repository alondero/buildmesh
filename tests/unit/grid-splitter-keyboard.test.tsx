/**
 * The agent grid's splitters, operable by keyboard (issue #2424).
 *
 * `GridSplitter` draws the mesh-scoped grid: one column splitter per row
 * and one row splitter between the rows. Both used to answer the mouse
 * only. These tests drive the real component and assert what a user can
 * observe:
 *
 *   - each splitter is a named separator in the tab order, with an
 *     orientation and a range that states the minimum pane size the drag
 *     enforces;
 *   - the arrow keys move the splitter the same way a drag moves it;
 *   - the minimum pane size still holds — the keyboard path obeys the
 *     same floor as the drag;
 *   - a keyboard resize is persisted to the per-mesh layout store (the
 *     same place a dragged resize is persisted) and refits the terminals;
 *   - the pointer drag still works after a keyboard resize.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup, act } from '@testing-library/react';
import { GridSplitter } from '../../src/components/AgentNodeView/GridSplitter';
import { useGridLayoutStore } from '../../src/stores/gridLayoutStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { terminalManager } from '../../src/components/Terminal/Terminal';

// The node card pulls in the terminal tree; the grid's own contract is
// the splitter, so the card is stubbed to a marker element.
vi.mock('../../src/components/AgentNodeView/NodeCard', () => ({
  NodeCard: ({ nodeId }: { nodeId: number }) => <div data-testid="grid-node" data-node-id={nodeId} />,
}));

vi.mock('../../src/components/Terminal/Terminal', () => ({
  terminalManager: { fitAll: vi.fn() },
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

// Four nodes lay out as a 2×2 grid (two rows of two columns), so the
// grid has BOTH a column splitter in each row and a row splitter
// between them.
const NODES: AgentNode[] = [makeNode(1), makeNode(2), makeNode(3), makeNode(4)];

/**
 * The container's measured size. jsdom has no layout, so the grid would
 * fall back to its own default size; this stands in for the measurement
 * the browser makes. The DRAG path measures the border box minus the
 * grid's 8px of padding (1192×892), while the keyboard path works off
 * the measured content box (1200×900) — the same numbers the app sees.
 */
const BORDER_BOX = { width: 1200, height: 900 };
const CONTAINER_WIDTH = 1200;
const CONTAINER_HEIGHT = 900;

/** The minimum pane size for a two-pane row/column: 300px + its share
 *  of the 5px splitter handles. */
const MIN_PANE_PX = 300 + 5 / 2;

let rectSpy: ReturnType<typeof vi.spyOn>;

function renderGrid() {
  return render(<GridSplitter nodes={NODES} activityMembersByRoot={{}} />);
}

function columnSeparator(row: number): HTMLElement {
  return screen.getByRole('separator', { name: `Resize column 1 of 2 in row ${row}` });
}

function rowSeparator(): HTMLElement {
  return screen.getByRole('separator', { name: 'Resize row 1 of 2' });
}

beforeEach(() => {
  // jsdom has no layout, so the grid would fall back to its own default
  // size. Give it a real one so the pixel floor and the pixel steps
  // mean what they mean in the app.
  rectSpy = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(() => ({
    width: BORDER_BOX.width,
    height: BORDER_BOX.height,
    top: 0,
    left: 0,
    right: BORDER_BOX.width,
    bottom: BORDER_BOX.height,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  }));
  useGridLayoutStore.setState({ byMesh: {} });
  useMeshStore.setState({ selectedMeshId: 1 });
  useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
  useAgentNodeStore.setState({ activeNodeId: null });
  vi.mocked(terminalManager.fitAll).mockClear();
});

afterEach(() => {
  rectSpy.mockRestore();
  cleanup();
});

describe('the grid splitters (issue #2424)', () => {
  it('exposes a named, focusable separator for each column and row', () => {
    renderGrid();

    const column = columnSeparator(1);
    expect(column.getAttribute('aria-orientation')).toBe('vertical');
    expect(column.getAttribute('tabindex')).toBe('0');
    // Half of the container.
    expect(column.getAttribute('aria-valuenow')).toBe('600');
    // The minimum pane size (300px) plus this row's share of the
    // splitter handles.
    expect(Number(column.getAttribute('aria-valuemin'))).toBeGreaterThanOrEqual(MIN_PANE_PX);
    expect(Number(column.getAttribute('aria-valuemax'))).toBeLessThanOrEqual(CONTAINER_WIDTH);

    const row = rowSeparator();
    expect(row.getAttribute('aria-orientation')).toBe('horizontal');
    expect(row.getAttribute('aria-valuenow')).toBe('450');
    expect(Number(row.getAttribute('aria-valuemin'))).toBeGreaterThanOrEqual(MIN_PANE_PX);
  });

  it('resizes a column with the arrow keys and persists the layout', () => {
    renderGrid();

    fireEvent.keyDown(columnSeparator(1), { key: 'ArrowRight' });

    // 8px of a 1200px container is two thirds of a percentage point.
    expect(columnSeparator(1).getAttribute('aria-valuenow')).toBe('608');
    const layout = useGridLayoutStore.getState().byMesh[1];
    expect(layout.colWidths[0][0]).toBeCloseTo(50.6667, 3);
    expect(layout.colWidths[0][1]).toBeCloseTo(49.3333, 3);
  });

  it('resizes a row with the arrow keys and persists the layout', () => {
    renderGrid();

    fireEvent.keyDown(rowSeparator(), { key: 'ArrowDown' });

    expect(rowSeparator().getAttribute('aria-valuenow')).toBe('458');
    const layout = useGridLayoutStore.getState().byMesh[1];
    expect(layout.rowHeights[0]).toBeCloseTo(50.8889, 3);
    expect(layout.rowHeights[1]).toBeCloseTo(49.1111, 3);
  });

  it('refits the terminals when a keyboard resize completes', () => {
    renderGrid();
    expect(terminalManager.fitAll).not.toHaveBeenCalled();

    fireEvent.keyDown(columnSeparator(1), { key: 'ArrowRight' });

    expect(terminalManager.fitAll).toHaveBeenCalled();
  });

  it('refits the terminals when a keyboard row resize completes', () => {
    renderGrid();

    fireEvent.keyDown(rowSeparator(), { key: 'ArrowDown' });

    expect(terminalManager.fitAll).toHaveBeenCalled();
  });

  it('keeps the minimum pane size when the keyboard drives a column to its floor', () => {
    renderGrid();

    fireEvent.keyDown(columnSeparator(1), { key: 'End' });

    const layout = useGridLayoutStore.getState().byMesh[1];
    // The pane in front of the handle stops at the floor; the pane
    // behind it soaks up the slack.
    expect(layout.colWidths[0][0]).toBeCloseTo(25.2083, 3);
    expect(layout.colWidths[0][1]).toBeCloseTo(74.7917, 3);
    expect(Number(columnSeparator(1).getAttribute('aria-valuenow'))).toBeGreaterThanOrEqual(MIN_PANE_PX);
  });

  it('keeps the minimum pane size when the keyboard drives a row to its floor', () => {
    renderGrid();

    fireEvent.keyDown(rowSeparator(), { key: 'End' });

    const layout = useGridLayoutStore.getState().byMesh[1];
    expect(layout.rowHeights[0]).toBeCloseTo(33.6111, 3);
    expect(layout.rowHeights[1]).toBeCloseTo(66.3889, 3);
    expect(Number(rowSeparator().getAttribute('aria-valuenow'))).toBeGreaterThanOrEqual(MIN_PANE_PX);
  });

  it('resizes only the row that owns the column separator', () => {
    renderGrid();

    fireEvent.keyDown(columnSeparator(2), { key: 'ArrowRight' });

    const layout = useGridLayoutStore.getState().byMesh[1];
    expect(layout.colWidths[1][0]).toBeCloseTo(50.6667, 3);
    expect(layout.colWidths[0][0]).toBeCloseTo(50, 3);
  });

  it('still drags with the pointer after a keyboard resize', async () => {
    renderGrid();
    const separator = columnSeparator(1);

    fireEvent.keyDown(separator, { key: 'ArrowRight' });
    expect(separator.getAttribute('aria-valuenow')).toBe('608');

    fireEvent(separator, new MouseEvent('mousedown', { bubbles: true, cancelable: true, clientX: 100, clientY: 100 }));
    fireEvent(document, new MouseEvent('mousemove', { bubbles: true, clientX: 140, clientY: 100 }));
    // The grid's drag coalesces its moves through requestAnimationFrame.
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 60));
    });
    fireEvent(document, new MouseEvent('mouseup', { bubbles: true }));

    // The drag continues from the keyboard's value: +40px on top of 608.
    expect(separator.getAttribute('aria-valuenow')).toBe('648');
  });
});
