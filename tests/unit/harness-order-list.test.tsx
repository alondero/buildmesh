/**
 * Harness reorder UI (issue #573 / ADR-0016). The drag itself can't be fired
 * through dnd-kit in jsdom, so the reorder math is unit-tested via the pure
 * `reorderIds` helper and the rendering contract is pinned separately.
 */
import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { HarnessOrderList, reorderIds } from '../../src/components/AppSettings/HarnessOrderList';
import type { ProviderInfo } from '../../src/lib/tauri';

function provider(id: string, label: string, is_proxied = false): ProviderInfo {
  return {
    id,
    label,
    color: '#fff',
    icon: id,
    resumable: false,
    // Issue #575 — Spawn Options carry the full wire shape; the
    // HarnessOrderList now filters by `!is_proxied` so a Proxied
    // Provider row never appears as an orderable harness.
    harness_id: id,
    provider_id: null,
    is_proxied,
    group_key: id,
  };
}

describe('reorderIds', () => {
  it('moves an id later in the list', () => {
    expect(reorderIds(['a', 'b', 'c'], 'a', 'c')).toEqual(['b', 'c', 'a']);
  });

  it('moves an id earlier in the list', () => {
    expect(reorderIds(['a', 'b', 'c'], 'c', 'a')).toEqual(['c', 'a', 'b']);
  });

  it('is a no-op when active and over are the same', () => {
    expect(reorderIds(['a', 'b', 'c'], 'b', 'b')).toEqual(['a', 'b', 'c']);
  });

  it('is a no-op when an id is missing', () => {
    expect(reorderIds(['a', 'b'], 'a', 'z')).toEqual(['a', 'b']);
  });
});

describe('HarnessOrderList', () => {
  const providers = [
    provider('claude', 'Claude Code'),
    provider('codex', 'Codex'),
    provider('terminal', 'Terminal'),
  ];

  it('renders a draggable row per non-terminal harness, excluding Terminal', () => {
    render(<HarnessOrderList providers={providers} onReorder={() => {}} />);
    expect(screen.getByText('Claude Code')).toBeTruthy();
    expect(screen.getByText('Codex')).toBeTruthy();
    // Terminal is pinned last by the backend, so it's never an orderable row.
    expect(screen.queryByText('Terminal')).toBeNull();
    // Each row exposes a reorder grab handle.
    expect(screen.getByLabelText('Reorder Claude Code')).toBeTruthy();
    expect(screen.getByLabelText('Reorder Codex')).toBeTruthy();
  });

  it('renders nothing when fewer than two non-terminal harnesses exist', () => {
    const { container } = render(
      <HarnessOrderList
        providers={[provider('claude', 'Claude Code'), provider('terminal', 'Terminal')]}
        onReorder={() => {}}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  // Issue #575 — Proxied Provider rows are NOT orderable harnesses.
  // They cluster under their harness header in the rendered Spawn Menu
  // (e.g. "Claude Code → MiniMax / Kimi") but reordering them would
  // re-order the *provider half* of an arbitrary pairing, which is
  // meaningless. They must be hidden from this list.
  it('excludes Proxied Provider rows from the orderable list (issue #575)', () => {
    const { container } = render(
      <HarnessOrderList
        providers={[
          provider('claude', 'Claude Code'),
          provider('claude:minimax', 'MiniMax', true),
          provider('claude:kimi', 'Kimi', true),
          provider('codex', 'Codex'),
        ]}
        onReorder={() => {}}
      />,
    );
    // Only the two native harnesses are orderable.
    expect(screen.getByLabelText('Reorder Claude Code')).toBeTruthy();
    expect(screen.getByLabelText('Reorder Codex')).toBeTruthy();
    // Proxied rows do NOT have a drag handle.
    expect(screen.queryByLabelText('Reorder MiniMax')).toBeNull();
    expect(screen.queryByLabelText('Reorder Kimi')).toBeNull();
    // And they don't render at all (not even a non-draggable row).
    expect(container.querySelector('[data-spawn-id="claude:minimax"]')).toBeNull();
    expect(container.querySelector('[data-spawn-id="claude:kimi"]')).toBeNull();
  });
});
