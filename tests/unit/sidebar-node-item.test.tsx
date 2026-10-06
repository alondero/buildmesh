import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NodeItem } from '../../src/components/Sidebar/NodeItem';
import { getMeshColor } from '../../src/lib/meshColors';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 10,
    mesh_id: 3,
    name: 'node-a',
    path: '/tmp/m',
    branch: 'main',
    env: 'wsl',
    provider: 'anthropic',
    status: 'running',
    use_worktree: false,
    created_at: '2026-01-01',
    ...overrides,
  };
}

const meshColor = getMeshColor(3);

describe('NodeItem', () => {
  // The row owns its active bit via the store (issue #1748) — reset it so
  // an `activeNodeId` seeded by one test can't leak into the next.
  beforeEach(() => {
    useAgentNodeStore.setState({ activeNodeId: null });
  });

  it('renders the node name and a status icon with a label', () => {
    render(<NodeItem node={makeNode()} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(screen.getByText('node-a')).toBeTruthy();
    expect(screen.getByTitle('Running')).toBeTruthy();
  });

  it('exposes the node id for e2e selectors', () => {
    const { container } = render(<NodeItem node={makeNode({ id: 99 })} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(container.querySelector('[data-session-id="99"]')).toBeTruthy();
  });

  it('marks the active node with the accent border', () => {
    useAgentNodeStore.setState({ activeNodeId: 10 });
    const { container } = render(<NodeItem node={makeNode()} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(container.querySelector('[data-session-item]')!.className).toContain('border-accent-cyan/50');
  });

  it('calls onSelectNode with the row ids when clicked', async () => {
    const onSelectNode = vi.fn();
    render(<NodeItem node={makeNode()} meshColor={meshColor} onSelectNode={onSelectNode} onDeleteNode={() => {}} />);
    await userEvent.click(screen.getByText('node-a'));
    expect(onSelectNode).toHaveBeenCalledWith(10, 3);
  });

  it('calls onDeleteNode when the delete button is clicked', () => {
    const onDeleteNode = vi.fn();
    render(<NodeItem node={makeNode()} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={onDeleteNode} />);
    fireEvent.click(screen.getByTitle('Delete node'));
    expect(onDeleteNode).toHaveBeenCalledTimes(1);
  });

  it('draws each status as one glyph in the same size box so rows align down the sidebar', () => {
    // jsdom can't measure layout, so assert className structure. Every status
    // must share one fixed box; the shape (fill style) is what changes.
    const sizeBoxes = new Set<string>();
    for (const [status, label, shape] of [
      ['running', 'Running', 'solid'],
      ['idle', 'Idle', 'ring'],
      ['pending', 'Starting…', 'dashed'],
      ['awaiting_input', 'Needs attention', 'target'],
      ['error', 'Error', 'cross'],
      ['lost', 'Lost', 'slash'],
      ['suspended', 'Suspended', 'half'],
      ['ready', 'Ready', 'target'],
      ['completed', 'PR opened', 'target'],
      ['archived', 'Archived', 'thin'],
    ] as const) {
      const { container, unmount } = render(
        <NodeItem
          node={makeNode({ status })}
          meshColor={meshColor}
          onSelectNode={() => {}}
          onDeleteNode={() => {}}
        />,
      );
      const glyph = screen.getByRole('img', { name: label });
      expect(glyph.getAttribute('data-shape'), `status=${status}`).toBe(shape);
      expect(container.querySelectorAll('[data-testid="node-status-glyph"]'), `status=${status}`).toHaveLength(1);
      sizeBoxes.add(glyph.className.replace(/\b(status-\S+|text-\S+|animate-\S+)\b/g, '').replace(/\s+/g, ' ').trim());
      unmount();
    }
    expect(sizeBoxes.size).toBe(1);
    expect([...sizeBoxes][0]).toContain('h-5 w-5');
  });

  it('no longer draws a separate text status dot', () => {
    const { container } = render(<NodeItem node={makeNode()} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    for (const dot of ['●', '○', '◌', '✓', '✗', '⊘', '⏸']) {
      expect(container.textContent, dot).not.toContain(dot);
    }
  });

  // Issue #1293 — `hover:brightness-125` is `filter: brightness(...)`,
  // which creates a containing block for `position:fixed` descendants
  // and breaks any overlay mounted inside the row. The row now uses
  // CSS-variable-driven background swaps instead. Pin the absence of
  // the filter utility AND the presence of the variable hooks so a
  // future revert to `brightness` (or an "easier" Tailwind class) is
  // caught at unit-test time.
  describe('inactive row hover uses CSS variables, not filter (issue #1293)', () => {
    it('does not include any Tailwind brightness/filter utility on the row', () => {
      const { container } = render(
        <NodeItem
          node={makeNode()}
          meshColor={meshColor}

          onSelectNode={() => {}}
          onDeleteNode={() => {}}
        />,
      );
      const row = container.querySelector('[data-session-item]')!;
      // No `brightness-*` (filter-based) class — `filter` creates a
      // containing block for `position:fixed` and breaks overlays.
      expect(row.className).not.toMatch(/\bbrightness-/);
      // No explicit `filter-*` Tailwind utility either (defensive —
      // we only need to flag the brightness case but a future
      // filter-based hover would also retarget `fixed`).
      expect(row.className).not.toMatch(/\bfilter-/);
    });

    it('sets --mesh-bg and --mesh-bg-hover CSS variables on inactive rows', () => {
      // The hover style swaps `--mesh-bg` for `--mesh-bg-hover` via a
      // Tailwind arbitrary-value class. Reading the inline style is
      // the simplest regression pin — it doesn't depend on Tailwind's
      // JIT emitting the `bg-[var(--mesh-bg)]` rule.
      const { container } = render(
        <NodeItem
          node={makeNode()}
          meshColor={meshColor}

          onSelectNode={() => {}}
          onDeleteNode={() => {}}
        />,
      );
      const row = container.querySelector<HTMLElement>('[data-session-item]')!;
      // Both alphas are hex + 2 alpha bytes. `meshColor.hex` is
      // 7 chars (`#rrggbb`); the alphas `40` / `99` are 0x40 ≈ 25%
      // and 0x99 ≈ 60% — visibly brighter on hover without using
      // `filter`. The hex comes from `getMeshColor`, which is
      // deterministic for the test mesh id.
      expect(row.style.getPropertyValue('--mesh-bg')).toBe(`${meshColor.hex}40`);
      expect(row.style.getPropertyValue('--mesh-bg-hover')).toBe(`${meshColor.hex}99`);
    });

    it('does not set CSS variables on active rows (they use the accent border instead)', () => {
      // Active rows swap the `hover:brightness` for a cyan border and
      // don't need a mesh-tinted hover (their fill is the accent
      // treatment). Pin that the inline style stays clean so the
      // CSS-variable hover doesn't double-paint.
      useAgentNodeStore.setState({ activeNodeId: 10 });
      const { container } = render(
        <NodeItem
          node={makeNode()}
          meshColor={meshColor}
          onSelectNode={() => {}}
          onDeleteNode={() => {}}
        />,
      );
      const row = container.querySelector<HTMLElement>('[data-session-item]')!;
      expect(row.style.getPropertyValue('--mesh-bg')).toBe('');
      expect(row.style.getPropertyValue('--mesh-bg-hover')).toBe('');
    });

  });

  it('adds the Circuit orbit to the same glyph only while the node is piloted', () => {
    useAgentNodeStore.setState({ circuitOwnerships: {} });
    const { container, rerender } = render(
      <NodeItem node={makeNode()} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />,
    );
    expect(container.querySelector('[data-orbit]')).toBeNull();

    useAgentNodeStore.setState({ circuitOwnerships: {10: { node_id: 10, run_id: 2, circuit_id: 3, circuit_name: 'Review', state: 'running', parent_node_id: null }} });
    rerender(<NodeItem node={makeNode()} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(screen.getByRole('img', { name: 'Running. Circuit active' }).querySelector('[data-orbit="comet"]')).toBeTruthy();
    expect(container.querySelectorAll('[data-testid="node-status-glyph"]')).toHaveLength(1);
  });

  it('uses Circuit ownership for the terminal Done orbit', () => {
    useAgentNodeStore.setState({ circuitOwnerships: {
      10: { node_id: 10, run_id: 2, circuit_id: 3, circuit_name: 'Review', state: 'completed', parent_node_id: null },
    } });
    const { container } = render(<NodeItem node={makeNode({ status: 'completed' })} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(screen.getByRole('img', { name: 'PR opened. Circuit done' })).toBeTruthy();
    expect(container.querySelector('[data-orbit]')?.getAttribute('data-orbit')).toBe('closed');
  });

  it('does not suppress lost-conversation recovery for terminal Circuit history', () => {
    const node = makeNode({ status: 'suspended', cli_session_id: '' });
    useAgentNodeStore.setState({ circuitOwnerships: {
      10: { node_id: 10, run_id: 2, circuit_id: 3, circuit_name: 'Review', state: 'completed', parent_node_id: null },
    } });
    render(<NodeItem node={node} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(screen.getByRole('img', { name: 'Missing session ID' })).toBeTruthy();
  });

  it('uses the same waiting and failure presentations as the canvas header', () => {
    useAgentNodeStore.setState({ circuitOwnerships: {10: { node_id: 10, run_id: 2, circuit_id: 3, circuit_name: 'Review', state: 'running', parent_node_id: null }} });
    const { container, rerender } = render(<NodeItem node={makeNode({ status: 'awaiting_input' })} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(screen.getByRole('img', { name: 'Needs attention. Circuit waiting' })).toBeTruthy();
    expect(container.querySelector('[data-orbit]')?.getAttribute('data-orbit')).toBe('half');

    useAgentNodeStore.setState({ circuitOwnerships: {10: { node_id: 10, run_id: 2, circuit_id: 3, circuit_name: 'Review', state: 'failed', parent_node_id: null }} });
    rerender(<NodeItem node={makeNode()} meshColor={meshColor} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(screen.getByRole('img', { name: 'Running. Circuit needs attention' })).toBeTruthy();
    expect(container.querySelector('[data-orbit]')?.getAttribute('data-orbit')).toBe('dashed');
  });
});
