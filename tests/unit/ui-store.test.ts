import { describe, it, expect, beforeEach, vi } from 'vitest';
import { useUIStore, type DiffContext } from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';

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

  describe('View Modes (wayfinder #982 / #983)', () => {
    beforeEach(() => {
      localStorage.removeItem('buildmesh.view-mode');
      // meshStore FIRST: the uiStore mesh-subscription fires synchronously
      // on a selectedMeshId change and would otherwise clobber the viewMode
      // set below. uiStore's own setState is the final authority.
      useMeshStore.setState({ selectedMeshId: null });
      useUIStore.setState({ viewMode: 'all', lastNonSingleMode: 'all' });
    });

    it('switches the active mode with setViewMode', () => {
      useUIStore.getState().setViewMode('pinned');
      expect(useUIStore.getState().viewMode).toBe('pinned');
    });

    it('remembers the last non-single mode as modes change', () => {
      useUIStore.getState().setViewMode('mesh');
      expect(useUIStore.getState().lastNonSingleMode).toBe('mesh');
      useUIStore.getState().setViewMode('pinned');
      expect(useUIStore.getState().lastNonSingleMode).toBe('pinned');
    });

    it('entering Single preserves the mode it was entered from', () => {
      useUIStore.getState().setViewMode('mesh');
      useUIStore.getState().setViewMode('single');
      expect(useUIStore.getState().viewMode).toBe('single');
      expect(useUIStore.getState().lastNonSingleMode).toBe('mesh');
    });

    it('exitSingleMode returns to the mode Single was entered from (Escape path)', () => {
      useUIStore.getState().setViewMode('pinned');
      useUIStore.getState().setViewMode('single');
      useUIStore.getState().exitSingleMode();
      expect(useUIStore.getState().viewMode).toBe('pinned');
    });

    it('exitSingleMode is a no-op outside Single', () => {
      useUIStore.getState().setViewMode('mesh');
      useUIStore.getState().exitSingleMode();
      expect(useUIStore.getState().viewMode).toBe('mesh');
    });

    it('setViewMode is idempotent — no subscriber notification on a same-mode call', () => {
      // The meshStore sync subscription fires on every sidebar selection
      // change; without this guard a re-select of the same mesh would
      // re-notify every uiStore subscriber and re-render the canvas.
      useUIStore.setState({ viewMode: 'mesh' });
      let notifyCount = 0;
      const unsub = useUIStore.subscribe(() => { notifyCount += 1; });
      useUIStore.getState().setViewMode('mesh');
      unsub();
      expect(notifyCount).toBe(0);
      expect(useUIStore.getState().viewMode).toBe('mesh');
    });

    it('persists the mode to localStorage on change', () => {
      useUIStore.getState().setViewMode('pinned');
      expect(localStorage.getItem('buildmesh.view-mode')).toBe('pinned');
    });

    it('does not touch localStorage on a same-mode no-op', () => {
      useUIStore.setState({ viewMode: 'all' });
      localStorage.removeItem('buildmesh.view-mode');
      useUIStore.getState().setViewMode('all');
      expect(localStorage.getItem('buildmesh.view-mode')).toBeNull();
    });

    describe('sidebar sync (one filter, two controls)', () => {
      it('selecting a mesh switches the canvas to Mesh Grid', () => {
        useMeshStore.getState().selectMesh(7);
        expect(useUIStore.getState().viewMode).toBe('mesh');
      });

      it('clearing the selection (re-click deselect) switches to All Nodes', () => {
        useMeshStore.getState().selectMesh(7);
        expect(useUIStore.getState().viewMode).toBe('mesh');
        useMeshStore.getState().selectMesh(null);
        expect(useUIStore.getState().viewMode).toBe('all');
      });

      it('a mesh selection switches modes even out of Single', () => {
        // A sidebar mesh click always means "show me this mesh" (#983) —
        // Single offers no resistance.
        useUIStore.getState().setViewMode('single');
        useMeshStore.getState().selectMesh(7);
        expect(useUIStore.getState().viewMode).toBe('mesh');
      });

      it('a same-value selectMesh does not clobber the current mode', () => {
        // zustand notifies subscribers on every `set`, even unchanged ones —
        // the subscription's prevState comparison is what keeps a no-op
        // selectMesh(null) from yanking the user out of Pinned.
        useUIStore.setState({ viewMode: 'pinned' });
        useMeshStore.getState().selectMesh(null);
        expect(useUIStore.getState().viewMode).toBe('pinned');
      });
    });

    describe('boot derivation (ticket #983)', () => {
      // loadViewMode runs once at store-module creation, so these tests
      // re-import the store on a fresh module registry with localStorage
      // pre-seeded. The statically-imported store above is unaffected.
      it('boots into a valid persisted mode', async () => {
        localStorage.setItem('buildmesh.view-mode', 'pinned');
        vi.resetModules();
        const fresh = await import('../../src/stores/uiStore');
        expect(fresh.useUIStore.getState().viewMode).toBe('pinned');
        expect(fresh.useUIStore.getState().lastNonSingleMode).toBe('pinned');
      });

      it('a boot straight into Single remembers no grid mode — return target is all', async () => {
        localStorage.setItem('buildmesh.view-mode', 'single');
        vi.resetModules();
        const fresh = await import('../../src/stores/uiStore');
        expect(fresh.useUIStore.getState().viewMode).toBe('single');
        expect(fresh.useUIStore.getState().lastNonSingleMode).toBe('all');
      });

      it('an invalid persisted value derives from the mesh selection (null → all)', async () => {
        localStorage.setItem('buildmesh.view-mode', 'bogus');
        vi.resetModules();
        const fresh = await import('../../src/stores/uiStore');
        expect(fresh.useUIStore.getState().viewMode).toBe('all');
      });

      it('an absent persisted value derives from the mesh selection (mesh selected → mesh)', async () => {
        localStorage.removeItem('buildmesh.view-mode');
        vi.resetModules();
        const freshMesh = await import('../../src/stores/meshStore');
        freshMesh.useMeshStore.setState({ selectedMeshId: 7 });
        const fresh = await import('../../src/stores/uiStore');
        expect(fresh.useUIStore.getState().viewMode).toBe('mesh');
      });
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
