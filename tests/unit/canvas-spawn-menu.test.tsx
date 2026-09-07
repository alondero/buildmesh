/**
 * Issue #1536 — CanvasSpawnMenu lifecycle tests.
 *
 * Senior review caught a UI lockout trap in the modal's mount logic:
 * an earlier iteration unconditionally mounted a Modal that fell
 * back to the sidebar's selection / first mesh whenever the store
 * id was `null`, so closing the modal via Escape or backdrop would
 * set the id back to `null`, the fallback would re-resolve to a real
 * mesh, the modal would re-render, and the user would be unable to
 * dismiss it.
 *
 * The fix moves the resolution to the CALLER (AgentNodeView's
 * `onOpenSpawnMenu` callback always sets a concrete mesh id) and
 * makes the modal a pure prop-driven renderer that closes the
 * store flag if the requested mesh disappears. These tests pin the
 * lifecycle contract so a future "convenience" refactor doesn't
 * reintroduce the trap.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { CanvasSpawnMenu } from '../../src/components/AgentNodeView/CanvasSpawnMenu';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { Mesh } from '../../src/types/generated/Mesh';

// `GroupedProviderMenu` is the real spawn surface; mock it so the
// tests don't drag the IPC layer into a render that only exercises
// mount/unmount.
vi.mock('../../src/components/Providers/GroupedProviderMenu', () => ({
  GroupedProviderMenu: ({ onSelect }: { onSelect: (id: string, alt: boolean) => void }) => (
    <button type="button" data-testid="fake-spawn-pick" onClick={() => onSelect('claude', false)}>
      Pick Claude
    </button>
  ),
}));

const MESH: Mesh = {
  id: 7,
  name: 'Repo Seven',
  path: '/r7',
  layout: 'grid',
  position: 0,
  created_at: '2026-01-01T00:00:00Z',
  build_command: null,
  run_command: null,
  model: null,
  effort: null,
  use_worktree: false,
  worktree_mode: null,
  default_provider: null,
  base_ref: 'main',
  scratchpad: '',
  sandbox: false,
  pre_spawn_pool_size: 1,
  color: null,
  autopilot_enabled: false,
  autopilot_trigger_label: null,
  autopilot_concurrency_limit: 2,
  autopilot_provider: null,
  autopilot_action_on_success: null,
  root_build_command: null,
  root_run_command: null,
  autopilot_mode: 'issue_driven' as Mesh['autopilot_mode'],
  loop_initial_prompt: null,
  loop_suffix_prompt: null,
  loop_max_iterations: null,
  loop_interval_seconds: 0,
  loop_consecutive_failures: 0,
  harness_overrides: {},
  circuit_run_capacity: 2,
  worktree_directory: null,
};

beforeEach(() => {
  useUIStore.setState({ canvasSpawnMenuMeshId: null });
  useMeshStore.setState({ meshes: [], meshesById: new Map(), selectedMeshId: null });
  // Restore the real (non-mocked) `selectProviderForMesh` between tests
  // — `useAgentNodeStore.setState` patches are scoped to each test.
  useAgentNodeStore.setState({});
});

describe('CanvasSpawnMenu mount lifecycle (issue #1536)', () => {
  it('renders the spawn dialog for the requested mesh', () => {
    useMeshStore.setState({ meshes: [MESH], meshesById: new Map([[MESH.id, MESH]]) });
    useUIStore.setState({ canvasSpawnMenuMeshId: MESH.id });

    render(<CanvasSpawnMenu meshId={MESH.id} />);

    expect(screen.getByText(`Spawn agent in ${MESH.name}`)).toBeTruthy();
  });

  it('closes the store flag and bails when the requested mesh disappears between open and render', () => {
    // App.tsx stores `canvasSpawnMenuMeshId = 7` (e.g. from a click),
    // but the mesh was deleted between the click and the next render
    // (e.g. `deleteMesh` resolved between frames). The modal must NOT
    // fall back to the first mesh — it must reset the store flag so
    // the next open call re-resolves.
    useMeshStore.setState({ meshes: [], meshesById: new Map() });
    useUIStore.setState({ canvasSpawnMenuMeshId: 7 });

    render(<CanvasSpawnMenu meshId={7} />);

    expect(useUIStore.getState().canvasSpawnMenuMeshId).toBeNull();
    expect(screen.queryByText('Spawn agent in')).toBeNull();
  });

  it('selecting a provider closes the modal and delegates to selectProviderForMesh', async () => {
    useMeshStore.setState({ meshes: [MESH], meshesById: new Map([[MESH.id, MESH]]) });
    useUIStore.setState({ canvasSpawnMenuMeshId: MESH.id });
    const selectSpy = vi.fn().mockResolvedValue(undefined);
    useAgentNodeStore.setState({
      selectProviderForMesh: selectSpy as unknown as ReturnType<typeof useAgentNodeStore.getState>['selectProviderForMesh'],
    });

    render(<CanvasSpawnMenu meshId={MESH.id} />);

    // The mock fires synchronously — click triggers `onSelect` which
    // closes the store flag before awaiting `selectProviderForMesh`.
    const pickButton = screen.getByTestId('fake-spawn-pick');
    pickButton.click();

    expect(useUIStore.getState().canvasSpawnMenuMeshId).toBeNull();
    await Promise.resolve();
    expect(selectSpy).toHaveBeenCalledWith(7, 'Repo Seven', '/r7', 'claude', false);
  });
});
