/**
 * Issue #1748 — `NodeItem` must skip re-renders on unrelated parent updates.
 *
 * A re-rendered `MeshItem` hands every row fresh per-row closures, so the
 * row memo cannot rely on callback identity. This file proves (through the
 * real component, with no duplicated comparator logic) that a parent
 * re-render keeping the same node reference does not re-execute the row
 * body, while a genuinely changed node still does.
 *
 * Technique: `InlineEditableText` (the always-mounted name cell) is wrapped
 * in a counting pass-through around the real component. The counter ticks
 * exactly when the `NodeItem` body executes — a memo bail-out produces no
 * tick. (`Profiler` cannot observe this: it fires `onRender` whenever it
 * re-renders itself, even when a memo child below it bails out.)
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { ComponentProps } from 'react';
import { NodeItem } from '../../src/components/Sidebar/NodeItem';
import { getMeshColor } from '../../src/lib/meshColors';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

const { ietRenders } = vi.hoisted(() => ({ ietRenders: { count: 0 } }));

vi.mock('../../src/components/shared/InlineEditableText', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/components/shared/InlineEditableText')>();
  const RealInlineEditableText = actual.InlineEditableText;
  function CountingInlineEditableText(props: ComponentProps<typeof RealInlineEditableText>) {
    ietRenders.count += 1;
    return <RealInlineEditableText {...props} />;
  }
  return { ...actual, InlineEditableText: CountingInlineEditableText };
});

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 10,
    mesh_id: 3,
    name: 'node-a',
    path: '/tmp/my-mesh',
    branch: 'main',
    env: 'wsl',
    provider: 'anthropic',
    status: 'running',
    use_worktree: false,
    position: 0,
    is_pinned: false,
    created_at: '2026-01-01',
    ...overrides,
  };
}

beforeEach(() => {
  ietRenders.count = 0;
  seedAgentNodes([]);
  useAgentNodeStore.setState({ autopilotStates: {}, circuitOwnerships: {} });
});

describe('NodeItem memo (issue #1748)', () => {
  it('skips a parent re-render that keeps the same node reference', () => {
    const node = makeNode();
    seedAgentNodes([node]);
    const { rerender } = render(
      <NodeItem node={node} meshColor={getMeshColor(3)} isActive={false} onSelect={() => {}} onDelete={() => {}} />,
    );
    expect(screen.getByText('node-a')).toBeTruthy();
    const baseline = ietRenders.count;
    expect(baseline).toBeGreaterThan(0);

    // Same node reference, brand-new per-row closures — exactly what a
    // re-rendered MeshItem hands down when it maps its rows.
    rerender(
      <NodeItem node={node} meshColor={getMeshColor(3)} isActive={false} onSelect={() => {}} onDelete={() => {}} />,
    );

    expect(ietRenders.count).toBe(baseline);
  });

  it('still re-renders when its own node reference changes', () => {
    const node = makeNode();
    seedAgentNodes([node]);
    const { rerender } = render(
      <NodeItem node={node} meshColor={getMeshColor(3)} isActive={false} onSelect={() => {}} onDelete={() => {}} />,
    );
    const baseline = ietRenders.count;

    // Control against a degenerate always-skip comparator: a genuinely
    // changed node (new reference, as `patchAgentNode` produces) must render.
    rerender(
      <NodeItem
        node={{ ...node, status: 'awaiting_input' }}
        meshColor={getMeshColor(3)}
        isActive={false}
        onSelect={() => {}}
        onDelete={() => {}}
      />,
    );

    expect(ietRenders.count).toBe(baseline + 1);
  });
});
