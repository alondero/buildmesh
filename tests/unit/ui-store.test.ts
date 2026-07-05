import { describe, it, expect, beforeEach } from 'vitest';
import { useUIStore, type DiffContext } from '../../src/stores/uiStore';

// A representative single-file diff context (issue #379). The overlay needs
// the path + root to fetch, the node/mesh to label and auto-close, and the
// source to pick the diff baseline.
const DIFF_CTX: DiffContext = {
  filePath: 'src/foo.ts',
  rootPath: '/repo/worktrees/agent-1',
  nodeId: 7,
  meshId: 1,
  source: 'base',
};

describe('useUIStore', () => {
  beforeEach(() => {
    useUIStore.setState({
      probeOpen: false,
      probeTab: 'files',
      activeDiffFile: null,
    });
  });

  describe('maximizedNode (#65)', () => {
    beforeEach(() => {
      useUIStore.setState({ maximizedNodeId: null });
    });

    it('toggles a node into maximized state', () => {
      useUIStore.getState().toggleMaximizedNode(7);
      expect(useUIStore.getState().maximizedNodeId).toBe(7);
    });

    it('toggling the same node again restores the grid', () => {
      useUIStore.getState().toggleMaximizedNode(7);
      useUIStore.getState().toggleMaximizedNode(7);
      expect(useUIStore.getState().maximizedNodeId).toBe(null);
    });

    it('toggling a different node switches the solo target', () => {
      useUIStore.getState().toggleMaximizedNode(7);
      useUIStore.getState().toggleMaximizedNode(9);
      expect(useUIStore.getState().maximizedNodeId).toBe(9);
    });

    it('clearMaximizedNode exits maximize (Escape path)', () => {
      useUIStore.getState().toggleMaximizedNode(7);
      useUIStore.getState().clearMaximizedNode();
      expect(useUIStore.getState().maximizedNodeId).toBe(null);
    });
  });

  describe('Probe Panel (issue #373)', () => {
    beforeEach(() => {
      useUIStore.setState({
        probeOpen: false,
        probeTab: 'files',
        activeDiffFile: null,
      });
    });

    describe('toggleProbe', () => {
      it('opens the panel when closed', () => {
        useUIStore.getState().toggleProbe();
        expect(useUIStore.getState().probeOpen).toBe(true);
      });

      it('closes the panel when open', () => {
        useUIStore.getState().toggleProbe();
        useUIStore.getState().toggleProbe();
        expect(useUIStore.getState().probeOpen).toBe(false);
      });
    });

    describe('setProbeOpen', () => {
      // Explicit open/close for callers that know which side they want (e.g.
      // AgentChangesTab closes the probe after opening a file in the centre
      // overlay). `toggleProbe` would flip in the wrong direction if called
      // when the panel was already closed, so this setter is the safer
      // primitive — and it short-circuits the no-op write.
      it('opens a closed panel', () => {
        useUIStore.setState({ probeOpen: false });
        useUIStore.getState().setProbeOpen(true);
        expect(useUIStore.getState().probeOpen).toBe(true);
      });

      it('closes an open panel', () => {
        useUIStore.setState({ probeOpen: true });
        useUIStore.getState().setProbeOpen(false);
        expect(useUIStore.getState().probeOpen).toBe(false);
      });

      it('is a no-op when the panel is already in the requested state', () => {
        useUIStore.setState({ probeOpen: true });
        useUIStore.getState().setProbeOpen(true);
        expect(useUIStore.getState().probeOpen).toBe(true);
      });

      it('does not allocate a state object when the value would not change', () => {
        // The guard short-circuits before touching `set`, so a no-op call
        // must not bump the store's change counter. This is the property
        // AgentChangesTab relies on for the burst-of-clicks case (file
        // watcher refresh re-fires the click handler).
        useUIStore.setState({ probeOpen: false });
        const before = useUIStore.getState();
        useUIStore.getState().setProbeOpen(false);
        const after = useUIStore.getState();
        expect(after).toBe(before);
      });
    });

    describe('setProbeTab', () => {
      it('changes the active tab', () => {
        useUIStore.getState().setProbeTab('review');
        expect(useUIStore.getState().probeTab).toBe('review');
      });

      it('leaves activeDiffFile untouched when switching tabs (#379)', () => {
        // The Center Workspace Diff Overlay floats over the terminal grid and
        // is independent of the active tab, so switching tabs must NOT close
        // it — the user keeps the Probe on any tab while reviewing.
        useUIStore.getState().openDiff(DIFF_CTX);
        useUIStore.getState().setProbeTab('worktrees');
        expect(useUIStore.getState().activeDiffFile).toEqual(DIFF_CTX);
        useUIStore.getState().setProbeTab('properties');
        expect(useUIStore.getState().activeDiffFile).toEqual(DIFF_CTX);
      });
    });

    describe('openDiff (#379)', () => {
      it('sets the context and opens the panel without changing the tab', () => {
        useUIStore.setState({ probeOpen: false, probeTab: 'files' });
        useUIStore.getState().openDiff(DIFF_CTX);
        expect(useUIStore.getState().activeDiffFile).toEqual(DIFF_CTX);
        // The overlay is consumed in the center, so the Probe stays where it
        // was — the user can keep clicking files in the same tab.
        expect(useUIStore.getState().probeTab).toBe('files');
        expect(useUIStore.getState().probeOpen).toBe(true);
      });

      it('overwrites a previously-open diff', () => {
        useUIStore.getState().openDiff(DIFF_CTX);
        const next: DiffContext = { ...DIFF_CTX, filePath: 'src/new.ts' };
        useUIStore.getState().openDiff(next);
        expect(useUIStore.getState().activeDiffFile).toEqual(next);
      });
    });

    describe('closeDiff', () => {
      it('clears the active diff context', () => {
        useUIStore.getState().openDiff(DIFF_CTX);
        useUIStore.getState().closeDiff();
        expect(useUIStore.getState().activeDiffFile).toBeNull();
      });

      it('is a no-op when no diff is open', () => {
        expect(useUIStore.getState().activeDiffFile).toBeNull();
        useUIStore.getState().closeDiff();
        expect(useUIStore.getState().activeDiffFile).toBeNull();
      });
    });

    describe('openProbeTab (#376)', () => {
      // One-call helper so the sidebar "File Explorer" menu and the agent
      // node git-summary chip can both open the probe on a specific tab
      // without each one re-implementing "set tab + open if closed". The
      // "click active tab to collapse" UX is left to ProbePanel's own click
      // handler, so this stays a pure "make the tab visible" action.
      it('opens the panel on the requested tab when it is closed', () => {
        useUIStore.setState({ probeOpen: false, probeTab: 'files' });
        useUIStore.getState().openProbeTab('review');
        expect(useUIStore.getState().probeOpen).toBe(true);
        expect(useUIStore.getState().probeTab).toBe('review');
      });

      it('switches to a different tab while keeping the panel open', () => {
        useUIStore.setState({ probeOpen: true, probeTab: 'files' });
        useUIStore.getState().openProbeTab('review');
        expect(useUIStore.getState().probeOpen).toBe(true);
        expect(useUIStore.getState().probeTab).toBe('review');
      });

      it('is idempotent on the active tab (no toggle-off)', () => {
        // `openProbeTab` is "make this tab visible" — closing stays a
        // separate concern (the activity-bar's click handler does that),
        // so the second call must not collapse the panel.
        useUIStore.setState({ probeOpen: true, probeTab: 'review' });
        useUIStore.getState().openProbeTab('review');
        expect(useUIStore.getState().probeOpen).toBe(true);
        expect(useUIStore.getState().probeTab).toBe('review');
      });

      it('opens on the requested tab from a different starting tab', () => {
        useUIStore.setState({ probeOpen: false, probeTab: 'properties' });
        useUIStore.getState().openProbeTab('files');
        expect(useUIStore.getState().probeOpen).toBe(true);
        expect(useUIStore.getState().probeTab).toBe('files');
      });

      // Issue #378 — the GitHub Issues and Session History probe tabs share
      // the same openProbeTab action as the rest. Pin both values here so a
      // future union-narrowing refactor can't silently drop the GitHub
      // Issues / Session History entry points.
      it('opens the 🐙 Git Issues tab (#378)', () => {
        useUIStore.setState({ probeOpen: false, probeTab: 'files' });
        useUIStore.getState().openProbeTab('issues');
        expect(useUIStore.getState().probeOpen).toBe(true);
        expect(useUIStore.getState().probeTab).toBe('issues');
      });

      it('opens the 🕒 Session History tab (#378)', () => {
        useUIStore.setState({ probeOpen: false, probeTab: 'files' });
        useUIStore.getState().openProbeTab('sessions');
        expect(useUIStore.getState().probeOpen).toBe(true);
        expect(useUIStore.getState().probeTab).toBe('sessions');
      });
    });
  });
});
