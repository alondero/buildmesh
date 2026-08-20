/**
 * AgentChangesTab — issue #376. The Probe Panel's 🔍 tab body.
 *
 * Wraps the existing `AgentReviewPanel` (ADR 0005) with the active agent
 * node's id and resolved path. The component is gated by ProbeTabBody —
 * by the time it mounts, `activeNodeId` is guaranteed non-null and
 * `activePath` points at the node's worktree directory (or the mesh root
 * for a non-worktree node).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent, act } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { AgentChangesTab } from '../../src/components/Probe/AgentChangesTab';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useUIStore } from '../../src/stores/uiStore';
import type { DiffResult } from '../../src/lib/tauri';
import { resetPathInvalidatedCacheForTests } from '../../src/lib/pathInvalidatedCache';
import { GIT_CHANGED } from '../../src/lib/events';

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'grid',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const NODE: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo/worktrees/agent-1',
  branch: 'main',
  env: 'windows',
  provider: 'anthropic',
  status: 'running',
  use_worktree: true,
  position: 0,
  created_at: '2026-01-01',
};

const DIFF: DiffResult = {
  files: [
    {
      path: 'src/app.ts',
      hunks: [
        {
          old_start: 1,
          old_lines: 1,
          new_start: 1,
          new_lines: 2,
          lines: [
            { line_type: 'remove', content: 'old', old_num: 1, new_num: null },
            { line_type: 'add', content: 'new', old_num: null, new_num: 2 },
          ],
        },
      ],
    },
  ],
};

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'diff_node_against_base') return Promise.resolve(DIFF);
    if (cmd === 'list_directory') return Promise.resolve({ name: 'repo', path: '/repo', is_dir: true, children: [] });
    if (cmd === 'get_git_branch_status') return Promise.resolve(null);
    return Promise.resolve({});
  });
}

describe('AgentChangesTab (#376)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
    useMeshStore.setState({
      meshes: [MESH],
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useUIStore.setState({ probeOpen: true, probeTab: 'review', activeDiffFile: null });
    // The pathInvalidatedCache primitive is module-level — the global
    // setup also resets it, but explicitly here keeps the test
    // self-contained (#1165 — the new freshness-window state lives in
    // a WeakMap keyed on subscribers, so any leftover subscriber would
    // not affect us; we still reset for hygiene).
    resetPathInvalidatedCacheForTests();
  });

  it('renders the AgentReviewPanel summary for the focused node', async () => {
    render(<AgentChangesTab />);

    // The summary bar reports "1 file changed" once the diff lands.
    await waitFor(() => {
      expect(screen.getByText(/1 file changed/)).toBeTruthy();
    });
    // The diff payload reaches the backend with the nodeId.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('diff_node_against_base', { nodeId: NODE.id });
    });
  });

  it('uses the active node\'s worktree path as the root for the file-tree browse affordance', async () => {
    render(<AgentChangesTab />);

    // The collapsible "File Tree" button is part of AgentReviewPanel.
    await waitFor(() => {
      expect(screen.getByText('File Tree')).toBeTruthy();
    });
  });

  it('opens the center diff overlay with a base-source context when a file is expanded (#379)', async () => {
    render(<AgentChangesTab />);

    const openBtn = await screen.findByRole('button', {
      name: /open src\/app\.ts in the center diff overlay/i,
    });
    fireEvent.click(openBtn);

    // The focused node + mesh are captured as the lens; source is 'base'
    // (since-branching) to match the Agent Changes view.
    expect(useUIStore.getState().activeDiffFile).toEqual({
      filePath: 'src/app.ts',
      rootPath: NODE.path,
      nodeId: NODE.id,
      meshId: MESH.id,
      source: 'base',
    });
  });

// Wiring pin: PathHeader is mounted and uses the focused node's worktree
  // (not the mesh root), matching project-files-tab.test.tsx:136.
  it('renders an "Open in file explorer" button that calls open_in_file_manager with the active node path', async () => {
    render(<AgentChangesTab />);

    const openButton = screen.getByRole('button', { name: /open in file explorer/i });
    expect(openButton.querySelector('svg')).toBeTruthy();

    fireEvent.click(openButton);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('open_in_file_manager', {
        path: NODE.path,
      });
    });
  });

  it('collapses the originating FileDiffCard so the diff is not shown twice (#758)', async () => {
    // Agent Changes renders each changed file as an expanded FileDiffCard by
    // default, so the right-hand probe is already showing the diff inline.
    // Without a collapse on the originating card, opening the centre overlay
    // would leave BOTH the expanded card AND the overlay displaying the same
    // diff. FileDiffCard collapses itself when its "open in centre" button
    // fires; the probe stays open (other cards remain expanded), so #379's
    // "probe stays open and interactive" contract holds uniformly.
    render(<AgentChangesTab />);

    // Sanity: the diff body is visible before the click (defaultOpen=true).
    await screen.findByText('new');

    const openBtn = await screen.findByRole('button', {
      name: /open src\/app\.ts in the center diff overlay/i,
    });
    fireEvent.click(openBtn);

    // Overlay opens — #379 wiring preserved.
    expect(useUIStore.getState().activeDiffFile).toEqual({
      filePath: 'src/app.ts',
      rootPath: NODE.path,
      nodeId: NODE.id,
      meshId: MESH.id,
      source: 'base',
    });

    // Probe stays open — #379 contract preserved for Agent Changes too.
    expect(useUIStore.getState().probeOpen).toBe(true);

    // The originating card collapsed; its diff body is no longer in the DOM.
    expect(screen.queryByText('new')).toBeNull();
  });

  // Issue #1165 — Agent Changes probe throttling. The
  // `useGitPathInvalidation(rootPath, ..., { minRefetchIntervalMs:
  // 2_000 })` call in `AgentReviewPanel` collapses a burst of
  // `GIT_CHANGED` events (the backend coalescer emits up to ~2/s
  // during an agent edit burst) into ONE trailing refetch instead of
  // one per event. Without this throttle, every emit fires
  // `diff_node_against_base` (`commands/diff.rs:602`), which
  // `run_blocking`-walks the worktree via libgit2 + 3× syntect per
  // hunk — the #761/#762 starvation class the issue calls out.
  //
  // Pin: a burst of 5 events fires the IPC at most twice (1
  // mount-driven fetch + 1 trailing refetch), not 5 times.
  it('throttles GIT_CHANGED refetches during an agent edit burst (#1165)', async () => {
    // Use a slow-resolving mock so trailing fires don't pile up while
    // the test asserts — we want to count IPC *invocations*, not
    // resolutions.
    let resolveRefetch!: () => void;
    const refetchBlocker = new Promise<void>((r) => {
      resolveRefetch = r;
    });
    let refetchCount = 0;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'diff_node_against_base') {
        refetchCount++;
        if (refetchCount >= 2) {
          // The 2nd call (the trailing-driven one) is allowed to
          // resolve so the test can move on. The 1st is the mount
          // fetch — also resolves naturally.
          return Promise.resolve(DIFF);
        }
        return refetchBlocker.then(() => DIFF);
      }
      if (cmd === 'list_directory')
        return Promise.resolve({ name: 'repo', path: '/repo', is_dir: true, children: [] });
      if (cmd === 'get_git_branch_status') return Promise.resolve(null);
      return Promise.resolve({});
    });

    render(<AgentChangesTab />);

    // Mount-driven fetch settles. Reset the counter so the assertions
    // below only see calls driven by GIT_CHANGED events.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('diff_node_against_base', { nodeId: NODE.id });
    });
    const callsBeforeBurst = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'diff_node_against_base'
    ).length;

    // Burst of 5 GIT_CHANGED events within ~50 ms (the backend
    // coalescer's throttle is 500 ms, but the primitive is purely
    // event-driven — a JS-side burst of events from any source is
    // equally throttled by the freshness window).
    await act(async () => {
      for (let i = 0; i < 5; i++) {
        await emit(GIT_CHANGED, { path: NODE.path });
      }
    });

    // Immediately after the burst: ONE new invoke (the leading
    // event fired the callback, the rest were suppressed and armed
    // a trailing timer). Five events → at most one extra fetch.
    await waitFor(() => {
      const total = vi.mocked(invoke).mock.calls.filter(
        ([cmd]) => cmd === 'diff_node_against_base'
      ).length;
      expect(total).toBe(callsBeforeBurst + 1);
    });

    // The trailing fires 2 s after the leading call. Wait past the
    // window — the 2nd trailing-driven fetch is the only additional
    // call. Five events → at most two extra fetches total.
    await new Promise((r) => setTimeout(r, 2_500));
    resolveRefetch(); // let the trailing-driven promise resolve
    await waitFor(() => {
      const total = vi.mocked(invoke).mock.calls.filter(
        ([cmd]) => cmd === 'diff_node_against_base'
      ).length;
      expect(total).toBe(callsBeforeBurst + 2);
    });

    // Pin the unconditional ceiling: even after the trailing, the
    // total must be `mount + 2`, not `mount + 5` (the un-throttled
    // shape). One extra pause + a fourth "shouldn't fire" check
    // rules out a phantom third trailing.
    await new Promise((r) => setTimeout(r, 1_000));
    const finalCount = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'diff_node_against_base'
    ).length;
    expect(finalCount).toBe(callsBeforeBurst + 2);
  });
});
