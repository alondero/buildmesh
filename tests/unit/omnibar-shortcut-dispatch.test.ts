/**
 * Tests for the Omnibar shortcut dispatch (issue #1409).
 *
 * App.tsx owns the Tauri global-shortcut side (⌘/Ctrl+K and ⌘/Ctrl+P wired
 * through `useGlobalShortcuts`, whose own registration bookkeeping is pinned
 * in `tests/unit/use-global-shortcuts.test.ts`). The dispatch side — turning
 * the `open-omnibar` / `open-omnibar-commands` action names into store
 * mutations — is a pure mutator in `src/lib/omnibarShortcuts.ts` (the same
 * shape as `gridShortcuts.ts` / `awaitingInputShortcuts.ts`), which App.tsx's
 * `shortcut-triggered` handler calls. Testing the mutator directly exercises
 * the exact mapping the handler uses without mounting the whole App.
 */
import { describe, it, expect, beforeEach } from 'vitest';

import { handleOmnibarAction, isOmnibarAction } from '../../src/lib/omnibarShortcuts';
import { useOmnibarStore } from '../../src/stores/omnibarStore';

describe('isOmnibarAction (issue #1409 — App.tsx shortcut dispatch branch)', () => {
  it('recognizes the open-omnibar actions the Tauri bindings dispatch', () => {
    expect(isOmnibarAction('open-omnibar')).toBe(true);
    expect(isOmnibarAction('open-omnibar-commands')).toBe(true);
  });

  it('rejects every sibling shortcut action', () => {
    for (const action of ['new-agent', 'arrow-left', 'toggle-maximize-grid', 'cycle-grid-modes', 'jump-to-next-awaiting', '']) {
      expect(isOmnibarAction(action), action).toBe(false);
    }
  });
});

describe('handleOmnibarAction (issue #1409 — ⌘/Ctrl+K / ⌘/Ctrl+P dispatch)', () => {
  beforeEach(() => {
    useOmnibarStore.setState({ open: false, mode: 'files' });
  });

  it('open-omnibar opens the palette in files mode (⌘/Ctrl+K)', () => {
    handleOmnibarAction('open-omnibar');
    expect(useOmnibarStore.getState().open).toBe(true);
    expect(useOmnibarStore.getState().mode).toBe('files');
  });

  it('open-omnibar-commands opens the palette in commands mode (⌘/Ctrl+P)', () => {
    handleOmnibarAction('open-omnibar-commands');
    expect(useOmnibarStore.getState().open).toBe(true);
    expect(useOmnibarStore.getState().mode).toBe('commands');
  });

  it('open-omnibar re-seeds an already-open palette instead of stacking', () => {
    handleOmnibarAction('open-omnibar');
    handleOmnibarAction('open-omnibar-commands');
    expect(useOmnibarStore.getState().open).toBe(true);
    expect(useOmnibarStore.getState().mode).toBe('commands');
  });

  it('unrelated actions leave the palette untouched', () => {
    handleOmnibarAction('new-agent');
    expect(useOmnibarStore.getState().open).toBe(false);
    expect(useOmnibarStore.getState().mode).toBe('files');
  });
});
