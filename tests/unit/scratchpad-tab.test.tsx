/**
 * Tests for the Scratch Pad probe tab (📝) — issue / scratch-pad-probe.
 *
 * Pins the tab's core invariants:
 *   - appears in the activity bar with the right icon/label
 *   - loads the mesh's scratch pad from the backend on mount
 *   - typing debounces to a single `set_mesh_scratchpad` IPC ~500ms
 *     after the last change
 *   - the corner indicator transitions Saving… → Saved
 *   - a mesh switch flushes any in-flight debounced write to the
 *     *outgoing* mesh before loading the new one (otherwise the user
 *     would silently lose their last keystrokes)
 *   - backend errors on load or save surface inline (not silent)
 *
 * Test mechanics
 * --------------
 * Tests that use fake timers must NOT call `screen.findBy*` (the async
 * variant polls with internal `setTimeout` calls, which never fire
 * under fake timers and hang the test until its 5s timeout). The
 * pattern below is: pre-set `probeOpen: true, probeTab: 'scratchpad'`
 * so the tab is already mounted when we render, then use sync
 * `screen.getByLabelText('Scratch pad')` to grab the textarea, then
 * `act(async () => vi.advanceTimersByTimeAsync(...))` to flush the
 * debounce. React's `act()` advances internal timers and processes
 * microtasks together so the IPC `.then` resolves inside the same
 * `act` block.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, act, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';

function makeMesh(id: number, name: string): Mesh {
  return {
    id,
    name,
    path: `/repos/${name}`,
    layout: 'grid',
    position: id,
    created_at: '2026-01-01',
    build_command: null,
    run_command: null,
    model: null,
    effort: null,
    use_worktree: true,
    worktree_mode: null,
    default_provider: null,
    base_ref: 'origin/main',
    scratchpad: '',
    sandbox: false,
  };
}

const MESH_A = makeMesh(101, 'alpha');
const MESH_B = makeMesh(102, 'beta');

/**
 * Per-test scratch pad state. Tests can preload different content for
 * each mesh and assert against the IPC payloads that the tab emits.
 */
const scratchpadByMesh = new Map<number, string>([
  [MESH_A.id, 'pre-existing note for alpha'],
  [MESH_B.id, 'beta stuff\nline 2'],
]);

const saveCalls: Array<{ meshId: number; content: string }> = [];
let loadError = false;
let saveError = false;

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    switch (cmd) {
      case 'get_mesh_scratchpad': {
        const meshId = (args as { meshId: number })?.meshId;
        if (loadError) return Promise.reject(new Error('boom-load'));
        return Promise.resolve(scratchpadByMesh.get(meshId) ?? '');
      }
      case 'set_mesh_scratchpad': {
        const { meshId, content } = args as { meshId: number; content: string };
        if (saveError) return Promise.reject(new Error('boom-save'));
        saveCalls.push({ meshId, content });
        scratchpadByMesh.set(meshId, content);
        return Promise.resolve();
      }
      default:
        return Promise.resolve({});
    }
  });
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH_A, MESH_B],
    meshesById: new Map([
      [MESH_A.id, MESH_A],
      [MESH_B.id, MESH_B],
    ]),
    selectedMeshId: MESH_A.id,
  });
  useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
  // Reset per-test scratch pad state. MESH_A starts with a non-empty
  // preloaded value so the "loaded content visible" assertion is
  // meaningful; tests that want a clean start can mutate the map.
  scratchpadByMesh.set(MESH_A.id, 'pre-existing note for alpha');
  scratchpadByMesh.set(MESH_B.id, 'beta stuff\nline 2');
  saveCalls.length = 0;
  loadError = false;
  saveError = false;
  mockBackend();
});

afterEach(() => {
  cleanup();
});

/** Render the dock with the Scratch Pad tab already open. Bypasses
 *  the activity-bar click so the textarea is mounted synchronously,
 *  letting the test use `getByLabelText` instead of the async
 *  `findByLabelText` (which would hang under fake timers). */
function renderWithScratchpadOpen() {
  useUIStore.setState({ probeOpen: true, probeTab: 'scratchpad' });
  return render(<ProbePanel />);
}

describe('ScratchpadTab (issue / scratch-pad-probe)', () => {
  it('appears in the probe activity rail with the "Scratch Pad" accessible name', () => {
    const user = userEvent.setup();
    render(<ProbePanel />);
    // The rail is icon-only (post-revamp VS Code idiom): the accessible
    // name comes from the button's aria-label, and the glyph is an
    // aria-hidden SVG — there is no visible caption to assert.
    const button = screen.getByRole('button', { name: 'Scratch Pad' });
    expect(button).toBeTruthy();
    expect(button.getAttribute('title')).toBe('Scratch Pad');
    // Clicking the activity-rail button opens the dock on this tab.
    return user.click(button).then(() => {
      const header = screen.getByRole('region', { name: 'Probe panel' });
      expect(header.textContent).toContain('Scratch Pad');
    });
  });

  it('loads the active mesh\'s scratch pad into the textarea on mount', async () => {
    renderWithScratchpadOpen();
    const textarea = (await screen.findByLabelText(
      'Scratch pad',
    )) as HTMLTextAreaElement;
    expect(textarea.value).toBe('pre-existing note for alpha');
  });

  it('debounces typing into a single `set_mesh_scratchpad` IPC 500ms after the last keystroke', async () => {
    vi.useFakeTimers();
    try {
      renderWithScratchpadOpen();
      const textarea = screen.getByLabelText('Scratch pad') as HTMLTextAreaElement;
      // Replace the preloaded value in a single change event so the
      // fake-timer advance below deterministically controls the
      // debounce window (per-keystroke `userEvent.type` interacts
      // poorly with fake timers in this vitest version).
      fireEvent.change(textarea, { target: { value: 'abc' } });

      // No save should have fired yet — the debounce hasn't elapsed.
      expect(saveCalls).toHaveLength(0);

      // Advance past the 500ms debounce. The `act` wrapper flushes
      // the timer's setState + the awaited IPC's microtask together.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(600);
      });

      // Exactly one IPC, carrying the final text.
      expect(saveCalls).toHaveLength(1);
      expect(saveCalls[0]).toEqual({ meshId: MESH_A.id, content: 'abc' });
    } finally {
      vi.useRealTimers();
    }
  });

  it('flips the corner indicator Saving… → Saved across the debounce', async () => {
    vi.useFakeTimers();
    try {
      renderWithScratchpadOpen();
      const textarea = screen.getByLabelText('Scratch pad') as HTMLTextAreaElement;
      fireEvent.change(textarea, { target: { value: 'x' } });

      // Saving… is shown synchronously with the keystroke.
      expect(screen.getByText('Saving…')).toBeTruthy();
      expect(screen.queryByText('Saved')).toBeNull();

      // After the debounce + the awaited IPC, the indicator flips.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(600);
      });

      expect(screen.getByText('Saved')).toBeTruthy();
      expect(screen.queryByText('Saving…')).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('flushes a pending write to the outgoing mesh before loading the new mesh', async () => {
    vi.useFakeTimers();
    try {
      renderWithScratchpadOpen();
      const textarea = screen.getByLabelText('Scratch pad') as HTMLTextAreaElement;

      // Type into mesh A and switch meshes within the debounce window
      // — the outgoing save MUST land before the new mesh loads,
      // otherwise the user silently loses those keystrokes.
      fireEvent.change(textarea, { target: { value: 'mid-thought for alpha' } });
      // Debounce is still armed (no 500ms has elapsed).
      expect(saveCalls).toHaveLength(0);

      // Switch the active mesh. The dock stays open on the same tab
      // (Scratch Pad); the load effect re-runs with the new meshId.
      // The `act` wrapper flushes the React commit AND any microtasks
      // scheduled inside the cleanup, so the synchronous `flushPending`
      // call (which fires `setMeshScratchpad`) resolves within this
      // block too.
      await act(async () => {
        useMeshStore.getState().selectMesh(MESH_B.id);
        // Flush the IPC's promise chain (the load is a real
        // microtask, not a timer; `await Promise.resolve()` gives the
        // event loop one tick to drain it).
        await Promise.resolve();
        await Promise.resolve();
      });

      // The save was flushed synchronously on the effect cleanup
      // path, before the new mesh's load effect ran.
      expect(saveCalls).toHaveLength(1);
      expect(saveCalls[0]).toEqual({
        meshId: MESH_A.id,
        content: 'mid-thought for alpha',
      });

      // The textarea now shows MESH_B's preloaded content. Same
      // microtask-flush trick as above — the load is a real Promise
      // that resolves once `await Promise.resolve()` ticks.
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(textarea.value).toBe('beta stuff\nline 2');
    } finally {
      vi.useRealTimers();
    }
  });

  it('surfaces a "Save failed" indicator when the backend rejects', async () => {
    vi.useFakeTimers();
    try {
      saveError = true;
      renderWithScratchpadOpen();
      const textarea = screen.getByLabelText('Scratch pad') as HTMLTextAreaElement;
      fireEvent.change(textarea, { target: { value: 'will fail' } });

      await act(async () => {
        await vi.advanceTimersByTimeAsync(600);
      });

      // Issue #813 — shared `<SaveIndicator>` includes the error's
      // `.message` ("Save failed: <message>"); regex pins the
      // error-state transition without coupling to the suffix.
      expect(screen.getByText(/Save failed/i)).toBeTruthy();
    } finally {
      saveError = false;
      vi.useRealTimers();
    }
  });

  it('surfaces a "Load failed" indicator when the initial get rejects', async () => {
    loadError = true;
    renderWithScratchpadOpen();
    expect(await screen.findByText('Load failed')).toBeTruthy();
  });
});
