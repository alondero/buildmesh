import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NodeItem } from '../../src/components/Sidebar/NodeItem';
import { getMeshColor } from '../../src/lib/meshColors';
import type { AgentNode } from '../../src/stores/agentNodeStore';

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
  it('renders the node name and a status icon with a label', () => {
    render(<NodeItem node={makeNode()} meshColor={meshColor} isActive={false} onSelect={() => {}} onDelete={() => {}} />);
    expect(screen.getByText('node-a')).toBeTruthy();
    expect(screen.getByTitle('Running')).toBeTruthy();
  });

  it('exposes the node id for e2e selectors', () => {
    const { container } = render(<NodeItem node={makeNode({ id: 99 })} meshColor={meshColor} isActive={false} onSelect={() => {}} onDelete={() => {}} />);
    expect(container.querySelector('[data-session-id="99"]')).toBeTruthy();
  });

  it('marks the active node with the accent border', () => {
    const { container } = render(<NodeItem node={makeNode()} meshColor={meshColor} isActive onSelect={() => {}} onDelete={() => {}} />);
    expect(container.querySelector('[data-session-item]')!.className).toContain('border-accent-cyan/50');
  });

  it('calls onSelect when clicked', async () => {
    const onSelect = vi.fn();
    render(<NodeItem node={makeNode()} meshColor={meshColor} isActive={false} onSelect={onSelect} onDelete={() => {}} />);
    await userEvent.click(screen.getByText('node-a'));
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it('calls onDelete when the delete button is clicked', () => {
    const onDelete = vi.fn();
    render(<NodeItem node={makeNode()} meshColor={meshColor} isActive={false} onSelect={() => {}} onDelete={onDelete} />);
    fireEvent.click(screen.getByTitle('Delete node'));
    expect(onDelete).toHaveBeenCalledTimes(1);
  });

  it('renders the status dot in a fixed-size box so ●/○/⏸/✗ align in the sidebar', () => {
    // jsdom can't measure layout, so assert className structure. The dot
    // span must carry the same fixed-width/height classes regardless of
    // status; otherwise the sans stack renders the outline of ○ visibly
    // larger than the filled ●, misaligning the dots down the sidebar list.
    //
    // Iterate every SessionStatus so the regression guard covers the
    // `text-violet` (suspended) and `animate-pulse-fast` (awaiting_input)
    // branches too — not just the two colour-matched cases.
    const stripVariable = (cn: string) =>
      cn
        .replace(/status-\S+/g, '')
        .replace(/\btext-violet\b/g, '')
        .replace(/\banimate-pulse-fast\b/g, '')
        .replace(/\s+/g, ' ')
        .trim();

    const expected =
      'inline-flex h-3 w-3 shrink-0 items-center justify-center text-xs leading-none';

    for (const [status, label] of [
      ['running', 'Running'],
      ['idle', 'Idle'],
      ['awaiting_input', 'Needs attention'],
      ['error', 'Error'],
      ['suspended', 'Suspended'],
    ] as const) {
      const { unmount } = render(
        <NodeItem
          node={makeNode({ status })}
          meshColor={meshColor}
          isActive={false}
          onSelect={() => {}}
          onDelete={() => {}}
        />,
      );
      const wrapper = stripVariable(screen.getByTitle(label).className);
      expect(wrapper, `status=${status}`).toBe(expected);
      unmount();
    }
  });
});
