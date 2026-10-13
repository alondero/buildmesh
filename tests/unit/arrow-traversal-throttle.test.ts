import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import {
  _resetShortcutRateGatesForTests,
  dispatchShortcut,
} from '../../src/lib/shortcutDispatch';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { makeAgentNode } from './helpers/makeAgentNode';
import { seedAgentNodes } from './helpers/seedAgentNodes';

/**
 * Rate limiting on the shortcut table (issue #2240).
 *
 * Without a limiter, holding a key floods the handler with OS autorepeat
 * events (~30 Hz on Windows, ~15 Hz on macOS). The table declares two
 * different primitives per action and the distinction is behavioural, not
 * cosmetic:
 *
 *   - **cooldown** (one-shot toggles: new-agent, Alt+G, the view-mode cycle,
 *     the `?` cheatsheet) blocks EVERY press inside the window.
 *   - **throttle** (the four arrow directions) lets the leading press through
 *     and rate-limits the rest, because a held arrow key *should* repeat —
 *     just slower. Wrapping arrows in a cooldown instead would silently turn
 *     grid traversal into a one-shot move, which is exactly the regression
 *     this file pins.
 *
 * The old version asserted the wiring by reading `src/App.tsx` and slicing the
 * source between two comment markers. These cases fire the actions and read
 * the reported outcome.
 */
describe('shortcut rate limiting', () => {
  beforeEach(() => {
    _resetShortcutRateGatesForTests();
    vi.useFakeTimers();
    seedAgentNodes([
      makeAgentNode({ id: 1, mesh_id: 1, position: 0 }),
      makeAgentNode({ id: 2, mesh_id: 1, position: 1 }),
    ], 1);
    useMeshStore.setState({ selectedMeshId: null });
    useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
  });

  afterEach(() => {
    vi.useRealTimers();
    document.body.innerHTML = '';
  });

  it('blocks a second toggle inside the 300ms cooldown, then admits it', async () => {
    expect(dispatchShortcut('toggle-maximize-grid')).toBe('handled');
    expect(useUIStore.getState().viewMode).toBe('single');

    expect(dispatchShortcut('toggle-maximize-grid')).toBe('blocked-cooldown');
    // The blocked press must not have run the handler either.
    expect(useUIStore.getState().viewMode).toBe('single');

    // Async variant: the cooldown's unblock timer is scheduled from a promise
    // `.finally`, so the microtask has to flush before the timer exists.
    await vi.advanceTimersByTimeAsync(300);
    expect(dispatchShortcut('toggle-maximize-grid')).toBe('handled');
    expect(useUIStore.getState().viewMode).toBe('all');
  });

  it('throttles (does NOT cooldown) the arrow directions: the repeat gets through', () => {
    // Two nodes in one row: right → node 2, left → node 1. If the arrows were
    // wrapped in a cooldown the second press would be dropped and traversal
    // would stop dead after one move.
    expect(dispatchShortcut('arrow-right')).toBe('handled');
    expect(dispatchShortcut('arrow-right')).toBe('blocked-throttle');
    vi.advanceTimersByTime(200);
    expect(dispatchShortcut('arrow-right')).toBe('handled');
  });

  it('shares ONE throttle across the four arrow directions', () => {
    // The OS autorepeat flood is a property of the key being held, not of the
    // direction. Four separate limiters would let a held Left reset the budget
    // and starve Right.
    expect(dispatchShortcut('arrow-right')).toBe('handled');
    expect(dispatchShortcut('arrow-left')).toBe('blocked-throttle');
    expect(dispatchShortcut('arrow-up')).toBe('blocked-throttle');
    expect(dispatchShortcut('arrow-down')).toBe('blocked-throttle');
  });

  it('gates the Single-view exit behind the throttle too', () => {
    // Regression guard for the old source-slice assertion: if the throttle sat
    // below the "restore from Single" branch, the autorepeat flood would
    // re-fire the exit and stomp the grid the user just left.
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'all' });
    expect(dispatchShortcut('arrow-down')).toBe('handled');
    expect(useUIStore.getState().viewMode).toBe('all');

    expect(dispatchShortcut('arrow-down')).toBe('blocked-throttle');
    expect(useUIStore.getState().viewMode).toBe('all');
  });

  it('does not let a focus-blocked press burn the limiter budget', () => {
    // The limiter records a press the moment it is consulted, so the focus
    // gate has to run FIRST (it does, in `dispatchShortcut`). If it didn't, a
    // no-op press inside an inline rename would silently swallow the user's
    // next deliberate Alt+G within the window.
    const input = document.createElement('input');
    document.body.appendChild(input);
    input.focus();

    expect(dispatchShortcut('toggle-maximize-grid')).toBe('blocked-focus');
    input.blur();

    expect(dispatchShortcut('toggle-maximize-grid')).toBe('handled');
    expect(useUIStore.getState().viewMode).toBe('single');
  });
});