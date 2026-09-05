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
          isActive={false}
          onSelect={() => {}}
          onDelete={() => {}}
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
          isActive={false}
          onSelect={() => {}}
          onDelete={() => {}}
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
      const { container } = render(
        <NodeItem
          node={makeNode()}
          meshColor={meshColor}
          isActive
          onSelect={() => {}}
          onDelete={() => {}}
        />,
      );
      const row = container.querySelector<HTMLElement>('[data-session-item]')!;
      expect(row.style.getPropertyValue('--mesh-bg')).toBe('');
      expect(row.style.getPropertyValue('--mesh-bg-hover')).toBe('');
    });
  });
});
