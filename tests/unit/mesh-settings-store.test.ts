/**
 * `meshSettingsStore` tests — issue #2242.
 *
 * The per-Mesh editing session owns three things the Probe destinations used
 * to hold inline, and each is pinned here at the seam where it can regress
 * quietly:
 *
 *  1. **The load** — one `get_mesh_properties` whose result feeds the form,
 *     the worktree strategy and the directory resolution, applied only while
 *     it is still the newest request.
 *  2. **The mesh-switch guard** — a save that settles after the user has moved
 *     on describes a project they are no longer looking at, so its result is
 *     dropped. Before the store existed this rule lived in an `activeMeshIdRef`
 *     inside the tab and in an `AbortSignal` ref inside the scratch pad; two
 *     copies, no shared test. The consumer-tab tests still cover the rendered
 *     behaviour; the store tests own the rule itself.
 *  3. **The save-status machine** — `idle → saving → saved` with an
 *     auto-clearing window, and `saving → error` that persists until the next
 *     save starts (issue #729). This suite replaces the deleted
 *     `use-save-status` suite: the machine moved into the store, so its cases
 *     moved with it rather than leaving a second, hook-shaped copy to test.
 *
 * Test mechanics
 * --------------
 * `invoke` is mocked globally by `tests/setup/vitest.setup.ts`, so each test
 * supplies the handful of commands it cares about. Fake timers cover the
 * `saved` auto-clear window and the scratch pad's 500ms debounce, so no test
 * waits on wall clock.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  useMeshSettingsStore,
  formToWireBaseRef,
  wireToFormBaseRef,
  wireToFormMode,
} from '../../src/stores/meshSettingsStore';
import { useMeshStore } from '../../src/stores/meshStore';
import type { MeshRow } from '../../src/lib/tauri';
import type { DetectedProject } from '../../src/lib/projectPresets';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const SEED_CONFIG: MeshRow = {
  name: 'demo',
  build_command: 'npm run build',
  run_command: 'npm run dev',
  root_build_command: 'cargo build',
  root_run_command: 'cargo run',
  base_ref: 'HEAD',
  use_worktree: false,
  worktree_mode: 'detached',
  default_provider: 'anthropic',
  sandbox: true,
  pre_spawn_pool_size: 3,
  worktree_directory: null,
};

const SEED_DETECTED: DetectedProject = {
  preset_id: 'node',
  label: 'Node',
  node_scripts: { build: 'build', run: 'dev', has_tauri_cli: false },
};

const DIR_CONFIG = {
  mesh_directory: null,
  app_directory: 'D:/app-default',
  effective_directory: 'D:/app-default/.worktrees',
  source: 'app_default',
};

/** Default command table: the load's four reads resolve with the seed, and
 *  every write succeeds. Individual tests override the one command they
 *  exercise. */
function commandTable(overrides: Record<string, unknown> = {}) {
  return (cmd: string): unknown => {
    if (cmd in overrides) return overrides[cmd];
    switch (cmd) {
      case 'get_mesh_properties':
        return Promise.resolve(SEED_CONFIG);
      case 'detect_mesh_project':
        return Promise.resolve(SEED_DETECTED);
      case 'get_worktree_directory_config':
        return Promise.resolve(DIR_CONFIG);
      case 'get_app_preferences':
        return Promise.resolve({ worktree_directory: 'D:/app-default' });
      case 'get_mesh_scratchpad':
        return Promise.resolve('note text');
      default:
        return Promise.resolve(undefined);
    }
  };
}

function seed(name: string): void {
  useMeshStore.setState({
    meshes: [{ id: 11, name, path: '/m/demo' } as never],
    meshesById: new Map([[11, { id: 11, name, path: '/m/demo' } as never]]),
  });
}

/** Load mesh 11 and wait for the store to settle on it. */
async function loadSeedMesh(overrides: Record<string, unknown> = {}) {
  mockInvoke.mockImplementation(commandTable(overrides));
  seed('demo');
  await useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo' });
}

/** Reset the store AND its module-private timers. The save-status window and
 *  the scratch pad's pending write live outside the state object, so a state
 *  reset alone would leak a live timer into the next test. */
function resetSession() {
  useMeshSettingsStore.getState().flushScratchpad();
  useMeshSettingsStore.getState().resetSaveStatus();
  useMeshSettingsStore.setState({
    meshId: null,
    loadedMeshId: null,
    meshPath: null,
    loading: false,
    loadError: null,
    detected: null,
    form: {
      name: '',
      buildCommand: '',
      runCommand: '',
      rootBuildCommand: '',
      rootRunCommand: '',
      defaultProvider: '',
      sandbox: false,
    },
    strategy: {
      useWorktree: true,
      baseRef: 'fresh',
      worktreeMode: 'branched',
      preSpawnPoolSize: 0,
      worktreeDirectory: '',
      worktreeDirEffective: '',
      worktreeDirAppDefault: null,
    },
    strategyError: null,
    status: 'idle',
    statusError: null,
    poolCount: null,
    scratchpad: { meshId: null, text: '', loadError: null, loaded: false },
  });
}

beforeEach(() => {
  resetSession();
  vi.clearAllMocks();
  useMeshStore.setState({ meshes: [], meshesById: new Map(), error: null });
});

afterEach(() => {
  resetSession();
  vi.useRealTimers();
});

describe('wire ↔ form mappers', () => {
  it('maps the canonical wire values and collapses everything else to the default', () => {
    expect(wireToFormBaseRef('HEAD')).toBe('head');
    expect(wireToFormBaseRef('origin/main')).toBe('fresh');
    expect(wireToFormBaseRef(null)).toBe('fresh');
    expect(wireToFormMode('detached')).toBe('detached');
    // `worktree_mode` is NULL for every row written before #451; the form's
    // fallback is the backend's default, not `''`.
    expect(wireToFormMode(null)).toBe('branched');
    expect(wireToFormMode('anything-else')).toBe('branched');
  });

  it('maps the form back to the two values the backend accepts', () => {
    expect(formToWireBaseRef('head')).toBe('HEAD');
    expect(formToWireBaseRef('fresh')).toBe('origin/main');
  });
});

describe('load', () => {
  it('applies one read of the config across the form, the strategy and the directory', async () => {
    await loadSeedMesh();

    const { form, strategy, detected, loadedMeshId, loading, loadError } =
      useMeshSettingsStore.getState();
    expect(form).toEqual({
      name: 'demo',
      buildCommand: 'npm run build',
      runCommand: 'npm run dev',
      rootBuildCommand: 'cargo build',
      rootRunCommand: 'cargo run',
      defaultProvider: 'anthropic',
      sandbox: true,
    });
    expect(strategy.useWorktree).toBe(false);
    expect(strategy.baseRef).toBe('head');
    expect(strategy.worktreeMode).toBe('detached');
    expect(strategy.preSpawnPoolSize).toBe(3);
    // Issue #1519 — the override, the app default and the resolved effective
    // path hydrate in the same pass, so nothing shows the previous project's.
    expect(strategy.worktreeDirectory).toBe('');
    expect(strategy.worktreeDirAppDefault).toBe('D:/app-default');
    expect(strategy.worktreeDirEffective).toBe('D:/app-default/.worktrees');
    expect(detected).toEqual(SEED_DETECTED);
    expect(loadedMeshId).toBe(11);
    expect(loading).toBe(false);
    expect(loadError).toBeNull();
    // One read, not one per section.
    const propertyReads = mockInvoke.mock.calls.filter(([c]) => c === 'get_mesh_properties');
    expect(propertyReads).toHaveLength(1);
  });

  it('dispatches every read before any of them resolves (max latency, not the sum)', async () => {
    let resolveConfig!: (c: MeshRow) => void;
    let resolveDetected!: (d: DetectedProject) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_properties') return new Promise<MeshRow>((r) => { resolveConfig = r; });
      if (cmd === 'detect_mesh_project') {
        return new Promise<DetectedProject>((r) => { resolveDetected = r; });
      }
      return commandTable()(cmd);
    });

    const pending = useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo' });
    await Promise.resolve();

    // If the reads were sequential only the first would be in flight here.
    const dispatched = mockInvoke.mock.calls.map(([c]) => c);
    expect(dispatched).toContain('get_mesh_properties');
    expect(dispatched).toContain('detect_mesh_project');
    expect(dispatched).toContain('get_worktree_directory_config');

    resolveConfig(SEED_CONFIG);
    resolveDetected(SEED_DETECTED);
    await pending;
  });

  it('falls back through config name → mesh name → folder name', async () => {
    mockInvoke.mockImplementation(commandTable({
      get_mesh_properties: { ...SEED_CONFIG, name: null },
    }));
    seed('from-store');
    await useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo', fallbackName: 'from-store' });
    expect(useMeshSettingsStore.getState().form.name).toBe('from-store');

    await useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo' });
    expect(useMeshSettingsStore.getState().form.name).toBe('demo');
  });

  it('clears the previous project’s drafts the moment a load for the next one starts', async () => {
    await loadSeedMesh();
    useMeshSettingsStore.getState().editForm({ buildCommand: 'edited in flight' });

    let resolveNext!: (c: MeshRow) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_properties') return new Promise<MeshRow>((r) => { resolveNext = r; });
      return commandTable()(cmd);
    });
    const pending = useMeshSettingsStore
      .getState()
      .load({ meshId: 12, meshPath: '/m/other' });
    await Promise.resolve();

    // Loading mesh 12 under mesh 11's values would edit the wrong project.
    const state = useMeshSettingsStore.getState();
    expect(state.meshId).toBe(12);
    expect(state.loadedMeshId).toBeNull();
    expect(state.form.buildCommand).toBe('');
    expect(state.loading).toBe(true);

    resolveNext(SEED_CONFIG);
    await pending;
    expect(useMeshSettingsStore.getState().loadedMeshId).toBe(12);
  });

  it('drops a load that resolves after the user switched projects', async () => {
    mockInvoke.mockImplementation(commandTable());
    let resolveStale!: (c: MeshRow) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_properties') {
        return cmd === 'get_mesh_properties' && staleFirst
          ? new Promise<MeshRow>((r) => { resolveStale = r; })
          : Promise.resolve(SEED_CONFIG);
      }
      return commandTable()(cmd);
    });
    let staleFirst = true;

    const stale = useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo' });
    await Promise.resolve();
    staleFirst = false;
    await useMeshSettingsStore.getState().load({ meshId: 12, meshPath: '/m/other' });
    resolveStale(SEED_CONFIG);
    await stale;

    // Mesh 11's config must not paint over mesh 12's form.
    const state = useMeshSettingsStore.getState();
    expect(state.meshId).toBe(12);
    expect(state.loadedMeshId).toBe(12);
  });

  it('retires the previous read when the SAME project is loaded again', async () => {
    // A retry (or a path edit) re-fires the load for the mesh already on
    // screen, so a mesh-id comparison alone would let the older response win.
    let resolveFirst!: (c: MeshRow) => void;
    let call = 0;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_properties') {
        call += 1;
        if (call === 1) return new Promise<MeshRow>((r) => { resolveFirst = r; });
        return Promise.resolve({ ...SEED_CONFIG, name: 'second read' });
      }
      return commandTable()(cmd);
    });

    const stale = useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo' });
    await Promise.resolve();
    await useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo' });
    resolveFirst({ ...SEED_CONFIG, name: 'first read' });
    await stale;

    expect(useMeshSettingsStore.getState().form.name).toBe('second read');
  });

  it('records a failed load and leaves the session unloaded', async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_properties') return Promise.reject(new Error('disk gone'));
      return commandTable()(cmd);
    });

    await useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo' });

    const state = useMeshSettingsStore.getState();
    expect(state.loadError).toBe('disk gone');
    expect(state.loading).toBe(false);
    expect(state.loadedMeshId).toBeNull();
  });

  it('degrades to a null detection rather than failing the whole load', async () => {
    await loadSeedMesh({ detect_mesh_project: Promise.reject(new Error('no package.json')) });

    const state = useMeshSettingsStore.getState();
    expect(state.detected).toBeNull();
    expect(state.loadedMeshId).toBe(11);
    expect(state.loadError).toBeNull();
  });
});

describe('save — mesh-switch guard (issue #2242)', () => {
  it('reports a successful save for the current project', async () => {
    await loadSeedMesh();
    await useMeshSettingsStore.getState().save('buildCommand', 'npm run build:prod');

    const state = useMeshSettingsStore.getState();
    expect(state.status).toBe('saved');
    expect(state.statusError).toBeNull();
    expect(state.form.buildCommand).toBe('npm run build:prod');
    expect(mockInvoke).toHaveBeenCalledWith('update_mesh_column', {
      meshId: 11,
      column: 'build_command',
      value: 'npm run build:prod',
    });
  });

  it('surfaces a rejected save and keeps the typed value', async () => {
    await loadSeedMesh({ update_mesh_column: Promise.reject(new Error('readonly fs')) });

    await useMeshSettingsStore.getState().save('runCommand', 'npm run serve');

    const state = useMeshSettingsStore.getState();
    expect(state.status).toBe('error');
    expect(state.statusError).toBe('readonly fs');
    // The form mirrors intent: reverting would silently undo the edit.
    expect(state.form.runCommand).toBe('npm run serve');
  });

  it('drops a save that resolves after the user switched projects', async () => {
    await loadSeedMesh();
    let resolveStale!: () => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'update_mesh_column') return new Promise<void>((r) => { resolveStale = r; });
      return commandTable()(cmd);
    });
    const stale = useMeshSettingsStore.getState().save('buildCommand', 'in flight');

    // Switch to another project and let ITS load settle.
    await useMeshSettingsStore.getState().load({ meshId: 12, meshPath: '/m/other' });
    resolveStale();
    await stale;

    // "Saved" for mesh 11 would describe a project the user is not looking at.
    const state = useMeshSettingsStore.getState();
    expect(state.status).toBe('idle');
    expect(state.statusError).toBeNull();
    expect(state.form.buildCommand).toBe(SEED_CONFIG.build_command);
  });

  it('drops a save that rejects after the user switched projects, keeping the log', async () => {
    await loadSeedMesh();
    let rejectStale!: (e: Error) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'update_mesh_column') return new Promise<void>((_r, rej) => { rejectStale = rej; });
      return commandTable()(cmd);
    });
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const stale = useMeshSettingsStore.getState().save('buildCommand', 'in flight');

    await useMeshSettingsStore.getState().load({ meshId: 12, meshPath: '/m/other' });
    rejectStale(new Error('too late'));
    await stale;

    const state = useMeshSettingsStore.getState();
    expect(state.status).toBe('idle');
    expect(state.statusError).toBeNull();
    // Audit trail survives even though the banner stays clean.
    expect(consoleError).toHaveBeenCalled();
    consoleError.mockRestore();
  });

  it('drops a strategy save that rejects after the user switched projects', async () => {
    await loadSeedMesh();
    let rejectStale!: (e: Error) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'update_worktree_base_ref') return new Promise<void>((_r, rej) => { rejectStale = rej; });
      return commandTable()(cmd);
    });
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const stale = useMeshSettingsStore.getState().save('baseRef', 'fresh');

    await useMeshSettingsStore.getState().load({ meshId: 12, meshPath: '/m/other' });
    rejectStale(new Error('too late'));
    await stale;

    expect(useMeshSettingsStore.getState().strategyError).toBeNull();
    consoleError.mockRestore();
  });

  it('names the column that refused a strategy write', async () => {
    await loadSeedMesh({ update_mesh_use_worktree: Promise.reject(new Error('pool is warm')) });

    await useMeshSettingsStore.getState().save('useWorktree', true);

    expect(useMeshSettingsStore.getState().strategyError).toBe(
      'Failed to update use_worktree: pool is warm',
    );
  });

  it('clears both error channels when the next project loads', async () => {
    await loadSeedMesh({ update_mesh_use_worktree: Promise.reject(new Error('pool is warm')) });
    await useMeshSettingsStore.getState().save('useWorktree', true);
    expect(useMeshSettingsStore.getState().strategyError).not.toBeNull();
    mockInvoke.mockImplementation(commandTable({ update_mesh_column: Promise.reject(new Error('boom')) }));
    await useMeshSettingsStore.getState().save('buildCommand', 'x');
    expect(useMeshSettingsStore.getState().statusError).toBe('boom');

    mockInvoke.mockImplementation(commandTable());
    await useMeshSettingsStore.getState().load({ meshId: 12, meshPath: '/m/other' });

    // Both messages describe controls that are about to change under a new
    // project's header.
    const state = useMeshSettingsStore.getState();
    expect(state.strategyError).toBeNull();
    expect(state.status).toBe('idle');
    expect(state.statusError).toBeNull();
  });

  it('skips the write (and the "Saved" flicker) when the name is unchanged', async () => {
    await loadSeedMesh();
    mockInvoke.mockClear();

    await useMeshSettingsStore.getState().save('name', 'demo');

    expect(mockInvoke).not.toHaveBeenCalledWith('update_mesh_name', expect.anything());
    expect(useMeshSettingsStore.getState().status).toBe('idle');
  });

  it('clamps the pool size before it reaches the backend', async () => {
    await loadSeedMesh();
    mockInvoke.mockClear();

    await useMeshSettingsStore.getState().save('poolSize', 99);

    expect(mockInvoke).toHaveBeenCalledWith('update_mesh_pool_size', { meshId: 11, poolSize: 5 });
    expect(useMeshSettingsStore.getState().strategy.preSpawnPoolSize).toBe(5);
  });

  it('does not write while the session has not finished loading', async () => {
    let resolveConfig!: (c: MeshRow) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_properties') return new Promise<MeshRow>((r) => { resolveConfig = r; });
      return commandTable()(cmd);
    });
    const pending = useMeshSettingsStore.getState().load({ meshId: 11, meshPath: '/m/demo' });
    await Promise.resolve();
    mockInvoke.mockClear();

    await useMeshSettingsStore.getState().save('buildCommand', 'too early');

    expect(mockInvoke).not.toHaveBeenCalled();
    expect(useMeshSettingsStore.getState().status).toBe('idle');

    resolveConfig(SEED_CONFIG);
    await pending;
  });
});

describe('save-status machine (issue #729)', () => {
  it('auto-clears `saved` back to `idle`', async () => {
    vi.useFakeTimers();
    await loadSeedMesh();
    await useMeshSettingsStore.getState().save('buildCommand', 'x');
    expect(useMeshSettingsStore.getState().status).toBe('saved');

    await vi.advanceTimersByTimeAsync(1600);

    expect(useMeshSettingsStore.getState().status).toBe('idle');
  });

  it('does not auto-clear when a newer save has already started', async () => {
    vi.useFakeTimers();
    await loadSeedMesh();
    let resolveFirst!: () => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'update_mesh_column') return new Promise<void>((r) => { resolveFirst = r; });
      return commandTable()(cmd);
    });
    const first = useMeshSettingsStore.getState().save('buildCommand', 'one');
    resolveFirst();
    await first;
    expect(useMeshSettingsStore.getState().status).toBe('saved');

    // Kick off a second save inside the auto-clear window.
    let resolveSecond!: () => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'update_mesh_column') return new Promise<void>((r) => { resolveSecond = r; });
      return commandTable()(cmd);
    });
    const second = useMeshSettingsStore.getState().save('buildCommand', 'two');
    expect(useMeshSettingsStore.getState().status).toBe('saving');

    // The first window must not undo the second save's "Saving…".
    await vi.advanceTimersByTimeAsync(1600);
    expect(useMeshSettingsStore.getState().status).toBe('saving');

    resolveSecond();
    await second;
  });

  it('clears a prior error when the next save starts, and keeps the error until then', async () => {
    await loadSeedMesh({ update_mesh_column: Promise.reject(new Error('boom')) });
    await useMeshSettingsStore.getState().save('buildCommand', 'a');
    expect(useMeshSettingsStore.getState().statusError).toBe('boom');

    mockInvoke.mockImplementation(commandTable());
    const ok = useMeshSettingsStore.getState().save('buildCommand', 'b');
    expect(useMeshSettingsStore.getState().statusError).toBeNull();
    await ok;
    expect(useMeshSettingsStore.getState().status).toBe('saved');
  });

  it('`resetSaveStatus` cancels the pending auto-clear', async () => {
    vi.useFakeTimers();
    await loadSeedMesh();
    await useMeshSettingsStore.getState().save('buildCommand', 'x');

    useMeshSettingsStore.getState().resetSaveStatus();
    expect(useMeshSettingsStore.getState().status).toBe('idle');

    await vi.advanceTimersByTimeAsync(1600);
    expect(useMeshSettingsStore.getState().status).toBe('idle');
  });
});

describe('worktree directory save (issue #1519)', () => {
  it('clears the override to the inherited value when blanked, then re-reads the resolution', async () => {
    await loadSeedMesh();
    mockInvoke.mockClear();
    mockInvoke.mockImplementation(commandTable({
      get_worktree_directory_config: {
        ...DIR_CONFIG,
        mesh_directory: null,
        effective_directory: 'D:/app-default/.worktrees',
      },
    }));

    await useMeshSettingsStore.getState().save('worktreeDirectory', '   ');

    expect(mockInvoke).toHaveBeenCalledWith('update_mesh_worktree_directory', {
      meshId: 11,
      directory: null,
    });
    const { strategy } = useMeshSettingsStore.getState();
    expect(strategy.worktreeDirectory).toBe('');
    expect(strategy.worktreeDirEffective).toBe('D:/app-default/.worktrees');
    expect(strategy.worktreeDirAppDefault).toBe('D:/app-default');
  });

  it('stores a trimmed override and shows the backend-resolved effective path', async () => {
    await loadSeedMesh();
    mockInvoke.mockImplementation(commandTable({
      get_worktree_directory_config: {
        mesh_directory: '  D:/override  ',
        app_directory: 'D:/app-default',
        effective_directory: 'D:/override',
        source: 'mesh',
      },
    }));

    await useMeshSettingsStore.getState().save('worktreeDirectory', '  D:/override  ');

    expect(mockInvoke).toHaveBeenCalledWith('update_mesh_worktree_directory', {
      meshId: 11,
      directory: 'D:/override',
    });
    // The backend's copy wins over the raw input so the display cannot drift
    // from the precedence rule.
    expect(useMeshSettingsStore.getState().strategy.worktreeDirectory).toBe('D:/override');
    expect(useMeshSettingsStore.getState().strategy.worktreeDirEffective).toBe('D:/override');
  });
});

describe('pool readiness badge', () => {
  it('records the count for the session’s project and ignores a late read for a departed one', async () => {
    await loadSeedMesh();
    let resolveCount!: (n: number) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_pool_count') return new Promise<number>((r) => { resolveCount = r; });
      return commandTable()(cmd);
    });

    useMeshSettingsStore.getState().refreshPoolCount(11);
    await useMeshSettingsStore.getState().load({ meshId: 12, meshPath: '/m/other' });
    resolveCount(4);

    // The badge belongs to mesh 12 now; mesh 11's in-flight count must not
    // claim it.
    expect(useMeshSettingsStore.getState().poolCount).toBeNull();
  });

  it('clears the badge when no project is selected', () => {
    useMeshSettingsStore.setState({ poolCount: 3 });
    useMeshSettingsStore.getState().refreshPoolCount(null);
    expect(useMeshSettingsStore.getState().poolCount).toBeNull();
  });
});

describe('scratch pad session', () => {
  // Each case uses its OWN mesh id: `lib/tauri`'s `getMeshScratchpad` keeps a
  // per-mesh promise cache for the life of the module (issue #405 — concurrent
  // readers de-dupe onto the in-flight promise, a rejection evicts the slot).
  // That is real production behaviour, so the suite works with it rather than
  // trying to reset a module-private cache from outside.
  it('loads the note and enables editing only for the loaded project', async () => {
    mockInvoke.mockImplementation(commandTable());

    useMeshSettingsStore.getState().loadScratchpad(11);
    await vi.waitFor(() => expect(useMeshSettingsStore.getState().scratchpad.loaded).toBe(true));

    const { scratchpad } = useMeshSettingsStore.getState();
    expect(scratchpad.meshId).toBe(11);
    expect(scratchpad.text).toBe('note text');
    expect(scratchpad.loadError).toBeNull();
  });

  it('keeps the editor disabled when the note could not be read', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_scratchpad') return Promise.reject(new Error('no such row'));
      return commandTable()(cmd);
    });

    useMeshSettingsStore.getState().loadScratchpad(21);
    await vi.waitFor(() =>
      expect(useMeshSettingsStore.getState().scratchpad.loadError).toBe('no such row'),
    );
    // An edit written against a row we never read would overwrite it blind.
    expect(useMeshSettingsStore.getState().scratchpad.loaded).toBe(false);
    consoleError.mockRestore();
  });

  it('ignores an edit until the note has been read', async () => {
    let resolveRead!: (t: string) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_mesh_scratchpad') return new Promise<string>((r) => { resolveRead = r; });
      return commandTable()(cmd);
    });

    useMeshSettingsStore.getState().loadScratchpad(31);
    useMeshSettingsStore.getState().editScratchpad('typed before load');

    expect(useMeshSettingsStore.getState().scratchpad.text).toBe('');
    resolveRead('note text');
    await vi.waitFor(() => expect(useMeshSettingsStore.getState().scratchpad.loaded).toBe(true));
  });

  it('collapses a burst of keystrokes into one debounced write', async () => {
    vi.useFakeTimers();
    mockInvoke.mockImplementation(commandTable());
    useMeshSettingsStore.getState().loadScratchpad(41);
    await vi.advanceTimersByTimeAsync(0);
    mockInvoke.mockClear();

    useMeshSettingsStore.getState().editScratchpad('he');
    useMeshSettingsStore.getState().editScratchpad('hello');
    expect(useMeshSettingsStore.getState().scratchpad.text).toBe('hello');
    expect(mockInvoke).not.toHaveBeenCalledWith('set_mesh_scratchpad', expect.anything());

    await vi.advanceTimersByTimeAsync(600);

    const writes = mockInvoke.mock.calls.filter(([c]) => c === 'set_mesh_scratchpad');
    expect(writes).toHaveLength(1);
    expect(writes[0][1]).toEqual({ meshId: 41, content: 'hello' });
  });

  it('flushes a pending draft to the outgoing project when the user switches', async () => {
    vi.useFakeTimers();
    mockInvoke.mockImplementation(commandTable());
    useMeshSettingsStore.getState().loadScratchpad(51);
    await vi.advanceTimersByTimeAsync(0);
    mockInvoke.mockClear();

    useMeshSettingsStore.getState().editScratchpad('unsaved tail');
    // Switching projects inside the debounce window must not lose the last
    // keystrokes, and they must land on mesh 51 — not on the project the user
    // just opened.
    useMeshSettingsStore.getState().loadScratchpad(52);
    await vi.advanceTimersByTimeAsync(0);

    expect(mockInvoke).toHaveBeenCalledWith('set_mesh_scratchpad', {
      meshId: 51,
      content: 'unsaved tail',
    });
  });

  it('drops a debounced write that resolves after the user switched projects', async () => {
    vi.useFakeTimers();
    mockInvoke.mockImplementation(commandTable());
    useMeshSettingsStore.getState().loadScratchpad(61);
    await vi.advanceTimersByTimeAsync(0);

    let resolveWrite!: () => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'set_mesh_scratchpad') return new Promise<void>((r) => { resolveWrite = r; });
      return commandTable()(cmd);
    });
    useMeshSettingsStore.getState().editScratchpad('in flight');
    await vi.advanceTimersByTimeAsync(600);
    expect(useMeshSettingsStore.getState().status).toBe('saving');

    useMeshSettingsStore.getState().loadScratchpad(62);
    resolveWrite();
    await vi.advanceTimersByTimeAsync(0);

    // The switch resets the indicator, and mesh 61's late "Saved" must not
    // put it back.
    expect(useMeshSettingsStore.getState().status).toBe('idle');
  });

  it('reports a debounced write failure for the project still on screen', async () => {
    vi.useFakeTimers();
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    mockInvoke.mockImplementation(commandTable());
    useMeshSettingsStore.getState().loadScratchpad(71);
    await vi.advanceTimersByTimeAsync(0);

    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'set_mesh_scratchpad') return Promise.reject(new Error('write failed'));
      return commandTable()(cmd);
    });
    useMeshSettingsStore.getState().editScratchpad('boom');
    await vi.advanceTimersByTimeAsync(600);

    expect(useMeshSettingsStore.getState().status).toBe('error');
    expect(useMeshSettingsStore.getState().statusError).toBe('write failed');
    consoleError.mockRestore();
  });

  it('clears the note and the indicator when the probe closes', () => {
    useMeshSettingsStore.setState({
      scratchpad: { meshId: 81, text: 'note', loadError: null, loaded: true },
      status: 'error',
      statusError: 'boom',
    });

    useMeshSettingsStore.getState().loadScratchpad(null);

    const { scratchpad, status, statusError } = useMeshSettingsStore.getState();
    expect(scratchpad).toEqual({ meshId: null, text: '', loadError: null, loaded: false });
    expect(status).toBe('idle');
    expect(statusError).toBeNull();
  });
});
