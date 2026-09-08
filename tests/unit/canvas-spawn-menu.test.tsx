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
 *
 * Reactivity hygiene (senior-review round 4):
 *   - `useProviderList` is mocked to a static list so the IPC-backed
 *     cache doesn't run an unmocked background fetch and trigger
 *     `publish()` during/after the test (the source of the four
 *     `act(...)` warnings).
 *   - `fireEvent.click` is used throughout instead of raw DOM
 *     `.click()` so React's batching is preserved.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { CanvasSpawnMenu } from '../../src/components/AgentNodeView/CanvasSpawnMenu';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { Mesh } from '../../src/types/generated/Mesh';
import type { SpawnOption } from '../../src/lib/groups';

// Mock the shared provider cache (issue #1502) so the test doesn't
// run an unmocked background IPC fetch. Without this, `useSyncExternalStore`
// would publish mid-test, generating the four `act(...)` stderr warnings
// that senior review caught.
vi.mock('../../src/hooks/useProviderList', () => ({
  useProviderList: () => [{
    id: 'claude',
    label: 'Claude Code',
    icon: '',
    harness_id: 'claude',
    provider_id: 'claude',
    is_proxied: false,
    group_key: 'claude',
    resumable: true,
  } satisfies SpawnOption],
  __resetSharedProviderListForTests: () => undefined,
}));

// `GroupedProviderMenu` is the real spawn surface; mock it so the
// tests don't drag the IPC layer into a render that only exercises
// mount/unmount. The mock exposes BOTH a plain click and an
// alt-click so the worktree-toggle path can be exercised.
vi.mock('../../src/components/Providers/GroupedProviderMenu', () => ({
  GroupedProviderMenu: ({ onSelect }: { onSelect: (id: string, alt: boolean) => void }) => (
    <div>
      <button
        type="button"
        data-testid="fake-spawn-pick"
        onClick={() => onSelect('claude', false)}
      >
        Pick Claude
      </button>
      <button
        type="button"
        data-testid="fake-spawn-pick-alt"
        onClick={() => onSelect('claude', true)}
      >
        Alt+Pick Claude
      </button>
    </div>
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
  // use_worktree=true so the alt-toggle test has something to invert
  // against (the inverting behavior is only meaningful when the mesh
  // has a non-default worktree setting).
  use_worktree: true,
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

    // fireEvent.click — raw .click() bypasses React's batching and
    // causes `act(...)` warnings (senior-review round 4).
    fireEvent.click(screen.getByTestId('fake-spawn-pick'));

    // Wait for the effect that fires the `selectProviderForMesh`
    // await to settle — the modal closes synchronously inside
    // `handleSelect` (before the await), but the spy assertion
    // happens after the microtask. waitFor drains the queue so
    // React commits and the spy sees the call.
    await waitFor(() => {
      expect(selectSpy).toHaveBeenCalledWith(7, 'Repo Seven', '/r7', 'claude', true);
    });
    expect(useUIStore.getState().canvasSpawnMenuMeshId).toBeNull();
  });

  it('alt+click toggles the worktree override against the mesh default', async () => {
    // Senior-review finding: altKey must TOGGLE the mesh's configured
    // `use_worktree`, not be passed through as the literal flag.
    useMeshStore.setState({ meshes: [MESH], meshesById: new Map([[MESH.id, MESH]]) });
    useUIStore.setState({ canvasSpawnMenuMeshId: MESH.id });
    const selectSpy = vi.fn().mockResolvedValue(undefined);
    useAgentNodeStore.setState({
      selectProviderForMesh: selectSpy as unknown as ReturnType<typeof useAgentNodeStore.getState>['selectProviderForMesh'],
    });

    render(<CanvasSpawnMenu meshId={MESH.id} />);

    fireEvent.click(screen.getByTestId('fake-spawn-pick-alt'));
    // MESH.use_worktree = true; altKey = true → invert → false
    await waitFor(() => {
      expect(selectSpy).toHaveBeenCalledWith(7, 'Repo Seven', '/r7', 'claude', false);
    });
  });
});
