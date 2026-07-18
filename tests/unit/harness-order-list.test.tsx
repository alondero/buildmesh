/**
 * Harness reorder UI (issue #573 / ADR-0016). The drag itself can't be fired
 * through dnd-kit in jsdom, so the reorder math is unit-tested via the pure
 * `reorderIds` helper and the rendering contract is pinned separately.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
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

// Issue #727 — keyboard a11y for the harness drag-reorder. The dnd-kit
// `DndContext` exposes the `KeyboardSensor` activator only through the
// handle's onKeyDown listener (wired via the `listeners` spread). Tests
// use fireEvent.keyDown on the focused handle; the sensor reads the key
// via its activator and updates dnd-kit internal state, which flips
// `aria-pressed` on the dragged row's handle to "true". `onDragEnd`
// fires when the user releases Space (or presses Escape to drop).
describe('HarnessOrderList — keyboard a11y (issue #727)', () => {
  afterEach(() => cleanup());

  const fourHarnesses = [
    provider('claude', 'Claude Code'),
    provider('codex', 'Codex'),
    provider('antigravity', 'Antigravity'),
    provider('opencode', 'OpenCode'),
  ];

  it('exposes a focusable drag handle per row with role=button and aria-roledescription=sortable', () => {
    render(<HarnessOrderList providers={fourHarnesses} onReorder={() => {}} />);
    const handle = screen.getByLabelText('Reorder Claude Code');
    // tabIndex=0 (from dnd-kit's useDraggable default + our explicit override).
    expect(handle.getAttribute('tabindex')).toBe('0');
    // role=button (dnd-kit default; our explicit override is idempotent).
    expect(handle.getAttribute('role')).toBe('button');
    // aria-roledescription=sortable — the WAI-ARIA description for a
    // sortable list item, overriding dnd-kit's "draggable" default.
    expect(handle.getAttribute('aria-roledescription')).toBe('sortable');
  });

  it('fires onReorder with the new order when Space picks up a handle and ArrowDown moves it before drop', async () => {
    // Walk through the full Space → ArrowDown → Space drop sequence.
    // dnd-kit's KeyboardSensor: Space activates the drag (aria-pressed
    // flips true), ArrowDown translates the active item to the next
    // slot (synthesising an `over` from `sortableKeyboardCoordinates`),
    // and the second Space finalises the move via `onDragEnd`.
    //
    // Activation is via the React `onKeyDown` listener dnd-kit spreads
    // onto the handle (`useSyntheticListeners`), so the first Space
    // fires on the handle. Once the drag is active, dnd-kit attaches
    // a document-level keydown listener via `setTimeout(...)` (to
    // avoid the same keydown both activating and ending the drag), so
    // ArrowDown and the drop Space fire on `document` AFTER a tick
    // boundary that flushes the deferred listener registration.
    //
    // The `sortableKeyboardCoordinates` getter walks siblings by
    // comparing `collisionRect.top` to each rect's `top`. jsdom returns
    // zero rects by default, which would leave the getter with no
    // "next" sibling and silently drop the move. Stub each row's
    // `getBoundingClientRect` with a strictly increasing `top` so the
    // coordinate getter can find the next row.
    //
    // We pass `code` explicitly because the dnd-kit activator reads
    // `event.nativeEvent.code` (NOT `event.key`). `KeyboardCode.Space`
    // maps to `"Space"`; passing `code: 'Space'` keeps the test
    // independent of jsdom's key→code table.
    const onReorder = vi.fn();
    const { container } = render(
      <HarnessOrderList providers={fourHarnesses} onReorder={onReorder} />,
    );
    // Stub rects: rows 0..3 with strictly increasing tops at 0, 50, 100, 150.
    // dnd-kit measures the `setNodeRef` target (the row div, not the handle
    // span) so we walk up from the `aria-roledescription="sortable"` handle
    // to the row that owns the ref.
    const handles = container.querySelectorAll('[aria-roledescription="sortable"]');
    expect(handles.length).toBe(4);
    handles.forEach((handle, i) => {
      // HarnessRow's row div carries `border border-border-subtle` —
      // the parent of the handle span. Walk up via the class marker so
      // a future nesting refactor that adds another wrapper still
      // finds the right node.
      let row: HTMLElement | null = handle as HTMLElement;
      while (row && !row.className.includes('border-border-subtle')) {
        row = row.parentElement;
      }
      expect(row).toBeTruthy();
      const top = i * 50;
      row!.getBoundingClientRect = function (this: HTMLElement) {
        return {
          width: 200,
          height: 50,
          top,
          left: 0,
          right: 200,
          bottom: top + 50,
          x: 0,
          y: top,
          toJSON() { return {}; },
        } as DOMRect;
      };
    });

    const claudeHandle = screen.getByLabelText('Reorder Claude Code') as HTMLElement;
    claudeHandle.focus();
    expect(document.activeElement).toBe(claudeHandle);

    // Pick up — handle-level listener.
    fireEvent.keyDown(claudeHandle, { key: ' ', code: 'Space' });
    // The pressed state should now be true — dnd-kit's draggable flips
    // `aria-pressed` while the drag is active.
    expect(claudeHandle.getAttribute('aria-pressed')).toBe('true');

    // Flush the dnd-kit setTimeout that defers the document-level
    // keydown listener registration (see KeyboardSensor.attach()).
    await new Promise(r => setTimeout(r, 0));

    // Move down by one slot — document-level listener (active drag).
    fireEvent.keyDown(document, { key: 'ArrowDown', code: 'ArrowDown' });

    // Drop. dnd-kit's KeyboardSensor treats Space (or Enter) as the
    // drop activator when the drag is active. We use a second Space
    // here — Enter also works but Space is the more conventional
    // "drag and drop" affordance.
    fireEvent.keyDown(document, { key: ' ', code: 'Space' });

    // onReorder should fire with Claude moved past Codex.
    expect(onReorder).toHaveBeenCalledTimes(1);
    expect(onReorder).toHaveBeenCalledWith(['codex', 'claude', 'antigravity', 'opencode']);
  });

  it('cancels the drag with Escape and does not invoke onReorder', async () => {
    // Escape drops the active item back to its original slot — dnd-kit's
    // KeyboardSensor dispatches an `onDragCancel`, which the harness
    // intentionally does not translate into an `onReorder` call (a
    // cancel is an explicit user "no", not a commit).
    const onReorder = vi.fn();
    render(<HarnessOrderList providers={fourHarnesses} onReorder={onReorder} />);
    const claudeHandle = screen.getByLabelText('Reorder Claude Code') as HTMLElement;
    claudeHandle.focus();

    fireEvent.keyDown(claudeHandle, { key: ' ', code: 'Space' });
    expect(claudeHandle.getAttribute('aria-pressed')).toBe('true');

    // Flush the dnd-kit setTimeout that defers the document-level
    // keydown listener registration before firing Escape.
    await new Promise(r => setTimeout(r, 0));

    // Same document-level listener as the reorder path — cancel is a
    // keyboard event on the active drag, not on the original activator.
    fireEvent.keyDown(document, { key: 'Escape', code: 'Escape' });

    // No reorder committed.
    expect(onReorder).not.toHaveBeenCalled();
    // Pressed state clears after cancel.
    expect(claudeHandle.getAttribute('aria-pressed')).not.toBe('true');
  });
});
