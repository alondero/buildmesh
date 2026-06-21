/**
 * Harness reorder UI (issue #573 / ADR-0016). The drag itself can't be fired
 * through dnd-kit in jsdom, so the reorder math is unit-tested via the pure
 * `reorderIds` helper and the rendering contract is pinned separately.
 */
import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { HarnessOrderList, reorderIds } from '../../src/components/AppSettings/HarnessOrderList';
import type { ProviderInfo } from '../../src/lib/tauri';

function provider(id: string, label: string): ProviderInfo {
  return { id, label, color: '#fff', icon: id, resumable: false };
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
});
