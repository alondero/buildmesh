import { describe, it, expect, beforeEach } from 'vitest';
import {
  requestWorktreeCloseAction,
  useWorktreeClosePromptStore,
} from '../../src/stores/worktreeClosePromptStore';
import type { WorktreeCloseSafety } from '../../src/lib/worktreeClose';

function makeSafety(overrides: Partial<WorktreeCloseSafety> = {}): WorktreeCloseSafety {
  return {
    worktree_path: '/a/.claude/worktrees/bold-keen-brook',
    has_uncommitted: false,
    has_unpushed: false,
    is_detached: false,
    ...overrides,
  };
}

describe('useWorktreeClosePromptStore', () => {
  beforeEach(() => {
    useWorktreeClosePromptStore.setState({ pending: null });
  });

  describe('request', () => {
    it('stores the pending prompt and exposes a resolve path', async () => {
      const safety = makeSafety({ has_uncommitted: true });
      const actionPromise = requestWorktreeCloseAction('alpha', safety);

      const pending = useWorktreeClosePromptStore.getState().pending;
      expect(pending?.nodeName).toBe('alpha');
      expect(pending?.safety).toBe(safety);

      useWorktreeClosePromptStore.getState().choose('remove');
      await expect(actionPromise).resolves.toBe('remove');
      expect(useWorktreeClosePromptStore.getState().pending).toBeNull();
    });

    // Regression test for issue #644: a back-to-back close (click × on A, then
    // × on B before A's dialog is dismissed) must not orphan A's resolver —
    // it must settle as 'cancel' so A's `deleteAgentNode` clears its
    // closingNodeIds flag instead of leaving the row stuck spinning.
    it('settles the prior pending promise as "cancel" when a new request comes in', async () => {
      const firstAction = requestWorktreeCloseAction('alpha', makeSafety({ has_uncommitted: true }));

      // Second request fires while the first prompt is still on screen.
      const secondAction = requestWorktreeCloseAction('beta', makeSafety({ has_unpushed: true }));

      // The first resolver must NOT be left dangling — its deleteAgentNode
      // caller awaits this to clear closingNodeIds. Pre-fix it never settled.
      await expect(firstAction).resolves.toBe('cancel');

      // The second prompt is the one currently displayed.
      const pending = useWorktreeClosePromptStore.getState().pending;
      expect(pending?.nodeName).toBe('beta');

      // The second prompt still resolves normally when the user picks an action.
      useWorktreeClosePromptStore.getState().choose('remove');
      await expect(secondAction).resolves.toBe('remove');
      expect(useWorktreeClosePromptStore.getState().pending).toBeNull();
    });

    // Pins the supersede-on-supersede contract: every prior pending in the
    // chain must settle as 'cancel', only the most-recent prompt resolves
    // via choose(). Catches a regression where the fix only checks the
    // immediate prior instead of every prior slot.
    it('cancels every prior in a 3-node chain, leaving only the latest prompt alive', async () => {
      const a = requestWorktreeCloseAction('alpha', makeSafety({ has_uncommitted: true }));
      const b = requestWorktreeCloseAction('beta', makeSafety({ has_uncommitted: true }));
      const c = requestWorktreeCloseAction('gamma', makeSafety({ has_uncommitted: true }));

      await expect(a).resolves.toBe('cancel');
      await expect(b).resolves.toBe('cancel');
      expect(useWorktreeClosePromptStore.getState().pending?.nodeName).toBe('gamma');

      useWorktreeClosePromptStore.getState().choose('remove');
      await expect(c).resolves.toBe('remove');
    });
  });

  describe('choose', () => {
    it('is a no-op when there is no pending prompt', () => {
      // Must not throw — choose() is wired to a backdrop click handler that
      // can fire after the store has already settled.
      expect(() => useWorktreeClosePromptStore.getState().choose('cancel')).not.toThrow();
    });
  });
});