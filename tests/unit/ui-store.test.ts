import { describe, it, expect, beforeEach } from 'vitest';
import { useUIStore } from '../../src/stores/uiStore';

describe('useUIStore', () => {
  beforeEach(() => {
    useUIStore.setState({
      changedFilesOpen: false,
      changedFilesNodeId: null,
      probeOpen: false,
      probeTab: 'files',
      activeDiffFile: null,
    });
  });

  describe('toggleChangedFiles', () => {
    it('opens panel for a node', () => {
      useUIStore.getState().toggleChangedFiles(1);
      expect(useUIStore.getState().changedFilesOpen).toBe(true);
      expect(useUIStore.getState().changedFilesNodeId).toBe(1);
    });

    it('closes panel when toggling same node', () => {
      useUIStore.getState().toggleChangedFiles(1);
      useUIStore.getState().toggleChangedFiles(1);
      expect(useUIStore.getState().changedFilesOpen).toBe(false);
      expect(useUIStore.getState().changedFilesNodeId).toBe(null);
    });

    it('switches to new node when different node clicked', () => {
      useUIStore.getState().toggleChangedFiles(1);
      useUIStore.getState().toggleChangedFiles(2);
      expect(useUIStore.getState().changedFilesOpen).toBe(true);
      expect(useUIStore.getState().changedFilesNodeId).toBe(2);
    });
  });

  describe('closeChangedFiles', () => {
    it('resets both state fields', () => {
      useUIStore.getState().toggleChangedFiles(5);
      useUIStore.getState().closeChangedFiles();
      expect(useUIStore.getState().changedFilesOpen).toBe(false);
      expect(useUIStore.getState().changedFilesNodeId).toBe(null);
    });
  });

  describe('setChangedFilesNodeId', () => {
    it('updates node when panel is open', () => {
      useUIStore.getState().toggleChangedFiles(1);
      useUIStore.getState().setChangedFilesNodeId(3);
      expect(useUIStore.getState().changedFilesNodeId).toBe(3);
      expect(useUIStore.getState().changedFilesOpen).toBe(true);
    });

    it('does nothing when panel is closed', () => {
      useUIStore.getState().setChangedFilesNodeId(3);
      expect(useUIStore.getState().changedFilesNodeId).toBe(null);
      expect(useUIStore.getState().changedFilesOpen).toBe(false);
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

    describe('setProbeTab', () => {
      it('changes the active tab', () => {
        useUIStore.getState().setProbeTab('review');
        expect(useUIStore.getState().probeTab).toBe('review');
      });

      it('does NOT round-trip activeDiffFile: switching to review → files → review leaves it cleared', () => {
        // Switching away from review is a one-way clear, even if the user
        // comes back to review later. The test name matches the
        // implementation: a switch-then-switch-back is NOT a round-trip —
        // the user is expected to re-pick a file from the tree.
        useUIStore.getState().openDiff('src/foo.ts');
        useUIStore.getState().setProbeTab('files');
        useUIStore.getState().setProbeTab('review');
        expect(useUIStore.getState().activeDiffFile).toBeNull();
      });

      it('clears activeDiffFile when switching away from review', () => {
        useUIStore.getState().openDiff('src/foo.ts');
        expect(useUIStore.getState().activeDiffFile).toBe('src/foo.ts');
        useUIStore.getState().setProbeTab('worktrees');
        expect(useUIStore.getState().activeDiffFile).toBeNull();
      });
    });

    describe('openDiff', () => {
      it('sets the file, jumps to the review tab, and opens the panel', () => {
        useUIStore.setState({ probeOpen: false, probeTab: 'files' });
        useUIStore.getState().openDiff('src/baz.ts');
        expect(useUIStore.getState().activeDiffFile).toBe('src/baz.ts');
        expect(useUIStore.getState().probeTab).toBe('review');
        expect(useUIStore.getState().probeOpen).toBe(true);
      });

      it('overwrites a previously-open diff', () => {
        useUIStore.getState().openDiff('src/old.ts');
        useUIStore.getState().openDiff('src/new.ts');
        expect(useUIStore.getState().activeDiffFile).toBe('src/new.ts');
      });
    });

    describe('closeDiff', () => {
      it('clears the active diff file', () => {
        useUIStore.getState().openDiff('src/foo.ts');
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
        // The legacy `toggleFileExplorer` semantics closed the panel on a
        // second click. `openProbeTab` is "make this tab visible" — closing
        // stays a separate concern (the activity-bar's click handler does
        // that), so the second call must not collapse the panel.
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
    });
  });
});
