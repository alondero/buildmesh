import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  useMeshPropertiesStore,
  type MeshConfig,
} from '../../src/stores/meshPropertiesStore';
import type { DetectedProject } from '../../src/lib/projectPresets';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const SEED_CONFIG: MeshConfig = {
  name: 'demo',
  build_command: 'npm run build',
  run_command: 'npm run dev',
  model: 'opus-4',
  effort: 'high',
  base_ref: 'origin/main',
  use_worktree: true,
  worktree_mode: 'branched',
  default_provider: 'anthropic',
};

const SEED_DETECTED: DetectedProject = {
  preset_id: 'node',
  label: 'Node',
  node_scripts: { build: 'build', run: 'dev', has_tauri_cli: false },
};

describe('useMeshPropertiesStore (issue #283)', () => {
  beforeEach(() => {
    useMeshPropertiesStore.setState({
      meshId: null,
      config: null,
      detected: null,
      loading: false,
      error: null,
    });
    vi.clearAllMocks();
  });

  describe('load', () => {
    it('populates config and detected from backend', async () => {
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_mesh_properties') return Promise.resolve(SEED_CONFIG);
        if (cmd === 'detect_mesh_project') return Promise.resolve(SEED_DETECTED);
        return Promise.resolve(undefined);
      });

      await useMeshPropertiesStore.getState().load(11, '/m');

      const state = useMeshPropertiesStore.getState();
      expect(state.meshId).toBe(11);
      expect(state.config).toEqual(SEED_CONFIG);
      expect(state.detected).toEqual(SEED_DETECTED);
      expect(state.loading).toBe(false);
      expect(state.error).toBeNull();
      expect(mockInvoke).toHaveBeenCalledWith('get_mesh_properties', { meshId: 11 });
      expect(mockInvoke).toHaveBeenCalledWith('detect_mesh_project', { meshPath: '/m' });
    });

    it('fires config + detection IN PARALLEL — the panel pays the max latency, not the sum', async () => {
      // Regression for /code-review finding: pre-fix awaited config then
      // detection sequentially. The panel "Loading..." overlay stays up for
      // the sum of both IPCs — on slow disks (antivirus scanning a mesh dir)
      // detection alone can be hundreds of ms; doubling the open latency was
      // visible. Pin parallelism by asserting both IPCs are dispatched
      // BEFORE either resolves.
      let resolveConfig!: (c: MeshConfig) => void;
      let resolveDetected!: (d: DetectedProject) => void;
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_mesh_properties') return new Promise((r) => { resolveConfig = r; });
        if (cmd === 'detect_mesh_project') return new Promise((r) => { resolveDetected = r; });
        return Promise.resolve(undefined);
      });

      const promise = useMeshPropertiesStore.getState().load(11, '/m');
      // Yield once so the load() function dispatches its awaits.
      await Promise.resolve();

      // Both IPCs are in-flight simultaneously — if load() were sequential,
      // only `get_mesh_properties` would have been called by now.
      const calls = mockInvoke.mock.calls.map(([c]) => c);
      expect(calls).toContain('get_mesh_properties');
      expect(calls).toContain('detect_mesh_project');

      resolveConfig(SEED_CONFIG);
      resolveDetected(SEED_DETECTED);
      await promise;
    });

    it('clears `config` on entry so a slow load cannot mirror the previous mesh into the new one', async () => {
      // Regression for /code-review finding: the load used to leave `config`
      // populated while a new load was in flight, so the panel form effect
      // would mirror mesh A's values into mesh B's form — a save() then
      // wrote A's value against meshId=B.
      useMeshPropertiesStore.setState({ meshId: 7, config: SEED_CONFIG });
      let resolveConfig!: (c: MeshConfig) => void;
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_mesh_properties') return new Promise((r) => { resolveConfig = r; });
        return Promise.resolve(null);
      });

      const promise = useMeshPropertiesStore.getState().load(99, '/b');
      // The instant a new load is dispatched, the previous config MUST be cleared.
      expect(useMeshPropertiesStore.getState().config).toBeNull();
      expect(useMeshPropertiesStore.getState().meshId).toBe(99);

      resolveConfig({ ...SEED_CONFIG, name: 'b' });
      await promise;
    });

    it('keeps `config` cleared when getMeshProperties rejects (no stale mirror on failure)', async () => {
      // Pre-fix: a failed load left the previous mesh's `config` in place
      // → the form effect mirrored A's values under B's title → user edits
      // were silently written cross-mesh.
      useMeshPropertiesStore.setState({ meshId: 7, config: SEED_CONFIG });
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_mesh_properties') return Promise.reject(new Error('mesh deleted'));
        return Promise.resolve(null);
      });

      await useMeshPropertiesStore.getState().load(99, '/b');

      const state = useMeshPropertiesStore.getState();
      expect(state.config).toBeNull();
      expect(state.error).toContain('mesh deleted');
      expect(state.loading).toBe(false);
    });

    it('records detection failure as null (not an error) — detection is best-effort', async () => {
      // The current panel quietly catches detection failures; preserve that
      // since detection is just a hint, not a load-blocking requirement.
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_mesh_properties') return Promise.resolve(SEED_CONFIG);
        if (cmd === 'detect_mesh_project') return Promise.reject(new Error('no scripts'));
        return Promise.resolve(undefined);
      });

      await useMeshPropertiesStore.getState().load(11, '/m');

      const state = useMeshPropertiesStore.getState();
      expect(state.config).toEqual(SEED_CONFIG);
      expect(state.detected).toBeNull();
      expect(state.error).toBeNull();
    });

    it('flips loading true while in flight, false on completion', async () => {
      let resolveConfig!: (c: MeshConfig) => void;
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_mesh_properties') return new Promise<MeshConfig>((r) => { resolveConfig = r; });
        return Promise.resolve(null);
      });

      const promise = useMeshPropertiesStore.getState().load(11, '/m');
      expect(useMeshPropertiesStore.getState().loading).toBe(true);

      resolveConfig(SEED_CONFIG);
      await promise;
      expect(useMeshPropertiesStore.getState().loading).toBe(false);
    });

    it('surfaces config-load failure on error and clears loading', async () => {
      mockInvoke.mockImplementation((cmd: string) => {
        if (cmd === 'get_mesh_properties') return Promise.reject(new Error('db locked'));
        return Promise.resolve(null);
      });

      await useMeshPropertiesStore.getState().load(11, '/m');

      const state = useMeshPropertiesStore.getState();
      expect(state.error).toContain('db locked');
      expect(state.loading).toBe(false);
    });
  });

  describe('save', () => {
    beforeEach(() => {
      useMeshPropertiesStore.setState({ meshId: 11, config: SEED_CONFIG });
    });

    it('routes simple agent fields through update_mesh_field', async () => {
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshPropertiesStore.getState().save('model', 'sonnet-4');

      expect(mockInvoke).toHaveBeenCalledWith('update_mesh_field', {
        meshId: 11, section: 'agent', key: 'model', value: 'sonnet-4',
      });
    });

    it('routes worktreeMode through update_mesh_field with snake_case key', async () => {
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshPropertiesStore.getState().save('worktreeMode', 'detached');

      expect(mockInvoke).toHaveBeenCalledWith('update_mesh_field', {
        meshId: 11, section: 'agent', key: 'worktree_mode', value: 'detached',
      });
    });

    it('routes defaultProvider through update_mesh_field with snake_case key', async () => {
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshPropertiesStore.getState().save('defaultProvider', 'agy');

      expect(mockInvoke).toHaveBeenCalledWith('update_mesh_field', {
        meshId: 11, section: 'agent', key: 'default_provider', value: 'agy',
      });
    });

    it('routes buildCommand to update_mesh_field with section=build, key=command', async () => {
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshPropertiesStore.getState().save('buildCommand', 'cargo build');

      expect(mockInvoke).toHaveBeenCalledWith('update_mesh_field', {
        meshId: 11, section: 'build', key: 'command', value: 'cargo build',
      });
    });

    it('routes runCommand to update_mesh_field with section=run, key=command', async () => {
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshPropertiesStore.getState().save('runCommand', 'cargo run');

      expect(mockInvoke).toHaveBeenCalledWith('update_mesh_field', {
        meshId: 11, section: 'run', key: 'command', value: 'cargo run',
      });
    });

    it('routes useWorktree through update_mesh_use_worktree (NOT update_mesh_field)', async () => {
      // Dedicated command exists because flipping use_worktree has DB-side
      // side-effects beyond a field write — routing through update_mesh_field
      // here would silently skip them.
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshPropertiesStore.getState().save('useWorktree', false);

      expect(mockInvoke).toHaveBeenCalledWith('update_mesh_use_worktree', {
        meshId: 11, useWorktree: false,
      });
      const fieldCalls = mockInvoke.mock.calls.filter(([cmd]) => cmd === 'update_mesh_field');
      expect(fieldCalls).toHaveLength(0);
    });

    it('routes baseRef through update_worktree_base_ref (NOT update_mesh_field)', async () => {
      // Same reason as useWorktree: base ref has side-effects in settings.json.
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshPropertiesStore.getState().save('baseRef', 'head');

      expect(mockInvoke).toHaveBeenCalledWith('update_worktree_base_ref', {
        meshId: 11, baseRef: 'head',
      });
    });

    it('skips effort persistence when value is empty (preserves panel behaviour)', async () => {
      // The current panel only saves effort when non-empty — empty means
      // "Not set" and the user shouldn't be able to overwrite the existing
      // value with a blank by mistake. Pin that here.
      await useMeshPropertiesStore.getState().save('effort', '');

      expect(mockInvoke).not.toHaveBeenCalled();
    });

    it('surfaces save failure on error', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('not writable'));

      await expect(
        useMeshPropertiesStore.getState().save('model', 'opus-4')
      ).rejects.toThrow('not writable');

      expect(useMeshPropertiesStore.getState().error).toContain('not writable');
    });

    it('no-ops when no mesh is loaded (defensive — should never happen in UI)', async () => {
      useMeshPropertiesStore.setState({ meshId: null, config: null });

      await useMeshPropertiesStore.getState().save('model', 'opus-4');

      expect(mockInvoke).not.toHaveBeenCalled();
    });
  });

  describe('applyPreset', () => {
    beforeEach(() => {
      useMeshPropertiesStore.setState({ meshId: 11, config: SEED_CONFIG });
    });

    it('fires the build + run update_mesh_field pair concurrently', async () => {
      mockInvoke.mockResolvedValue(undefined);

      await useMeshPropertiesStore.getState().applyPreset({
        id: 'node', label: 'Node', build: 'npm run build', run: 'npm run dev',
      });

      // Both fired, with the right sections.
      const calls = mockInvoke.mock.calls;
      expect(calls).toContainEqual(['update_mesh_field', {
        meshId: 11, section: 'build', key: 'command', value: 'npm run build',
      }]);
      expect(calls).toContainEqual(['update_mesh_field', {
        meshId: 11, section: 'run', key: 'command', value: 'npm run dev',
      }]);
    });

    it('rejects when one of the two field writes fails', async () => {
      mockInvoke.mockImplementation((_cmd: string, args: { section?: string }) => {
        if (args?.section === 'run') return Promise.reject(new Error('disk full'));
        return Promise.resolve(undefined);
      });

      await expect(
        useMeshPropertiesStore.getState().applyPreset({
          id: 'node', label: 'Node', build: 'b', run: 'r',
        })
      ).rejects.toThrow('disk full');
    });
  });

  describe('reset', () => {
    it('clears every panel-scoped field', async () => {
      useMeshPropertiesStore.setState({
        meshId: 11, config: SEED_CONFIG, detected: SEED_DETECTED,
        loading: false, error: 'whoops',
      });

      useMeshPropertiesStore.getState().reset();

      const state = useMeshPropertiesStore.getState();
      expect(state.meshId).toBeNull();
      expect(state.config).toBeNull();
      expect(state.detected).toBeNull();
      expect(state.error).toBeNull();
    });
  });
});
