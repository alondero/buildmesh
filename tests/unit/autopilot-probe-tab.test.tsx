/**
 * Tests for the Autopilot Probe tab — wayfinder #990, ticket #994.
 *
 * Strategy mirrors `mesh-properties-tab.test.tsx`: mount the full
 * `ProbePanel` with the Autopilot destination opened via `openProbeTab`
 * (the post-#1375 on-demand entry point), then assert on the rendered
 * form and on the IPC calls the tab fires. The mesh
 * store is seeded directly, the `invoke` mock is wired per-test, and
 * the `MeshRow` fixture carries the full v30 surface (issue-driven
 * columns + the six looping columns) so the tab's load effect sees
 * a realistic row.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import type { MeshRow } from '../../src/types/generated/MeshRow';
import type { LoopStatusDto } from '../../src/types/generated/LoopStatus';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { openProbeDestination } from './helpers/openProbeDestination';

// The Mesh store's `Mesh` is the full generated 14+ field wire type
// (wayfinder #990 ticket #991 added the six `loop_*` and the
// `autopilot_mode` discriminator to the row). The vitest TS config
// typechecks tests permissively, so a short literal is enough for the
// store to accept it; the runtime MeshRow returned to the tab carries
// every relevant field.
const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

/** Full v30 MeshRow baseline. Each test builds its own variant via
 *  `meshRow()` to flip the fields under test. Reuses the generated
 *  `MeshRow` type from `src/types/generated/MeshRow.ts` (CONTRIBUTING.md
 *  "Harness-enforced rules" + `docs/knowledge-primer.md` Anti-Patterns
 *  forbid hand-declaring a TS interface for a Rust wire type — the
 *  drift-gate on `src/types/generated/` can't see tests, so the only
 *  protection against silent drift is importing the generated type
 *  here). */
const BASE_MESH_ROW: MeshRow = {
  name: 'demo',
  build_command: null,
  run_command: null,
  model: null,
  effort: null,
  base_ref: 'origin/main',
  use_worktree: true,
  worktree_mode: null,
  default_provider: null,
  sandbox: false,
  pre_spawn_pool_size: 0,
  autopilot_enabled: false,
  autopilot_trigger_label: null,
  autopilot_concurrency_limit: 2,
  autopilot_provider: null,
  autopilot_action_on_success: null,
  root_build_command: null,
  root_run_command: null,
  autopilot_mode: 'issue_driven',
  loop_initial_prompt: null,
  loop_suffix_prompt: null,
  loop_max_iterations: null,
  loop_interval_seconds: 0,
  loop_consecutive_failures: 0,
};

function meshRow(overrides: Partial<MeshRow> = {}): MeshRow {
  // Keep `autopilot_mode`'s narrow union in the resulting object so the
  // generated-type contract holds (Partial widens it to `string`).
  const merged = { ...BASE_MESH_ROW, ...overrides } as MeshRow;
  return merged;
}

function mockBackend(
  row: MeshRow,
  opts: {
    rejectLoopConfig?: boolean;
    rejectSetEnabled?: boolean;
    /** Seed the runtime status the tab reads via `get_loop_status`. The
     *  `enabled` flag is stateful: a `set_mesh_autopilot_enabled` call flips
     *  it so a follow-up `get_loop_status` reflects Start/Stop (mirrors the
     *  real DB round-trip). Defaults derive from the row + a live-iteration
     *  of none. */
    loopStatus?: Partial<LoopStatusDto>;
    /** Provider catalogue the issue-driven branch renders via
     *  `groupByHarness(providers)`. Tests that exercise the new
     *  policy fields (#1013) opt in by passing an array — the default
     *  empty array keeps the looping-only path free of optgroups.
     *  Uses the generated `ProviderInfo` type (re-exported from
     *  `src/lib/tauri`) — same source the `MeshPropertiesTab`'s
     *  provider select consumes, so shape drift between tests and
     *  production code is caught at compile time. */
    providers?: ProviderInfo[];
    /** Compatibility verdict the AutopilotProbeTab UI consumes
     *  (issue #1152). Defaults to a fully-compatible verdict so
     *  existing tests don't need to know about the gate. Tests that
     *  exercise the disabled-controls behaviour override this with an
     *  `allowed: false` verdict + a reason list. */
    compatibility?: {
      allowed: boolean;
      reasons: Array<Record<string, unknown>>;
      resolved_harness_id: string | null;
      resolved_spawn_option: string | null;
      explicit_autopilot_provider: boolean;
    };
  } = {}
) {
  let enabled = opts.loopStatus?.enabled ?? row.autopilot_enabled;
  const activeIteration = opts.loopStatus?.active_iteration ?? null;
  const totalIterations = opts.loopStatus?.total_iterations ?? 0;

  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'get_mesh_properties') return Promise.resolve(row);
    if (cmd === 'get_loop_status') {
      const dto: LoopStatusDto = {
        enabled,
        active_iteration: activeIteration,
        total_iterations: totalIterations,
      };
      return Promise.resolve(dto);
    }
    if (cmd === 'get_autopilot_compatibility') {
      // Default to a fully-compatible verdict so the existing
      // AutopilotProbeTab tests can focus on the legacy behaviour.
      // Issue #1152-specific tests override this via `opts.compatibility`.
      return Promise.resolve(
        opts.compatibility ?? {
          allowed: true,
          reasons: [],
          resolved_harness_id: 'claude',
          resolved_spawn_option: 'claude',
          explicit_autopilot_provider: false,
        }
      );
    }
    if (cmd === 'list_providers') {
      // Empty by default — the looping branch never reads the
      // catalogue; tests that exercise the issue-driven "Autopilot
      // provider" select opt in via `opts.providers`.
      return Promise.resolve(opts.providers ?? []);
    }
    if (cmd === 'set_mesh_autopilot_enabled') {
      if (opts.rejectSetEnabled) {
        return Promise.reject(new Error('mock: rejected start/stop'));
      }
      enabled = Boolean(args?.enabled);
      return Promise.resolve();
    }
    if (
      cmd === 'update_mesh_loop_config' ||
      cmd === 'update_mesh_use_worktree' ||
      cmd === 'update_mesh_autopilot' ||
      cmd === 'update_mesh_column' ||
      cmd === 'delete_mesh'
    ) {
      if (opts.rejectLoopConfig && cmd === 'update_mesh_loop_config') {
        return Promise.reject(new Error('mock: rejected loop config save'));
      }
      return Promise.resolve();
    }
    // Default-fall-through to the command name so the test failure
    // surface shows what we hit unexpectedly.
    return Promise.resolve({ cmd });
  });
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  seedAgentNodes([]);
  useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
});

describe('AutopilotProbeTab (wayfinder #990 ticket #994)', () => {
  it('opens via openProbeTab and shows the mode toggle (issue-driven by default)', async () => {
    mockBackend(meshRow());
    openProbeDestination('autopilot');

    // The segmented control is always visible (both modes exist). The old
    // rail button carried the "Autopilot" name; with the rail gone (#1375)
    // the header label and the mode-toggle buttons are the naming surface.
    expect(
      (await screen.findByRole('button', { name: 'Looping', pressed: false }))
    ).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Issue-Driven' }).getAttribute('aria-pressed')).toBe('true');

    // In issue-driven mode (ticket #1013) the body is now the actual
    // Autopilot Policy form (master toggle + 4 fields), not the prose
    // pointer that #994 landed pre-#1013. The default fixture has
    // `autopilot_enabled = false`, so the master checkbox is in the
    // DOM unchecked and the four policy fields are gated behind it.
    // Looping controls and the loop-status badge stay hidden so a
    // future regression that bleed them in is caught here.
    expect((await screen.findByLabelText(/^Autopilot on/i))?.getAttribute('type')).toBe('checkbox');
    expect(screen.queryByLabelText('Trigger label')).toBeNull();
    expect(screen.queryByLabelText('Max concurrent autopilot nodes')).toBeNull();
    expect(screen.queryByLabelText('Autopilot provider')).toBeNull();
    expect(screen.queryByLabelText('On success')).toBeNull();
    expect(screen.queryByLabelText(/^Initial prompt/i)).toBeNull();
    expect(screen.queryByTestId('loop-status-badge')).toBeNull();
  });

  it('switching to Looping writes the full loop config atomically', async () => {
    mockBackend(meshRow());
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    await user.click(await screen.findByRole('button', { name: 'Looping' }));

    // update_mesh_loop_config is the ONE atomic write for ALL six
    // loop columns plus the mode discriminator. The default fixture's
    // null prompts + zero counters serialise to null / 0.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_loop_config', {
        meshId: 42,
        mode: 'looping',
        initialPrompt: null,
        suffixPrompt: null,
        maxIterations: null,
        intervalSeconds: 0,
        consecutiveFailures: 0,
      });
    });

    // Looping controls now render.
    expect(screen.getByLabelText(/^Initial prompt/i)).toBeTruthy();
    expect(screen.getByLabelText(/^Suffix prompt/i)).toBeTruthy();
    expect(screen.getByLabelText(/^Max iterations/i)).toBeTruthy();
    expect(screen.getByLabelText(/^Pause between/i)).toBeTruthy();
    expect(screen.getByLabelText(/^Auto-pause after/i)).toBeTruthy();
    expect(screen.getByLabelText(/^Run loop iterations in a worktree/i)).toBeTruthy();
  });

  it('initial prompt saves on blur, trimming blank text to null', async () => {
    mockBackend(meshRow({ autopilot_mode: 'looping' }));
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    const textarea = await screen.findByLabelText(/^Initial prompt/i);
    await user.type(textarea, 'Ship the top issue');
    fireEvent.blur(textarea);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'update_mesh_loop_config',
        expect.objectContaining({
          meshId: 42,
          mode: 'looping',
          initialPrompt: 'Ship the top issue',
          maxIterations: null,
          intervalSeconds: 0,
          consecutiveFailures: 0,
        })
      );
    });

    // Clear the prompt — whitespace-only must serialise to null, not
    // to the literal empty string (the backend's `clean` lambda would
    // reject "" anyway; doing it locally avoids the round-trip).
    fireEvent.change(textarea, { target: { value: '   ' } });
    fireEvent.blur(textarea);
    await waitFor(() => {
      expect(invoke).toHaveBeenLastCalledWith(
        'update_mesh_loop_config',
        expect.objectContaining({ initialPrompt: null })
      );
    });
  });

  it('numeric loop fields parse to integers; blank max iterations means continuous', async () => {
    mockBackend(meshRow({ autopilot_mode: 'looping' }));
    openProbeDestination('autopilot');

    const max = await screen.findByLabelText(/^Max iterations/i);
    const interval = screen.getByLabelText(/^Pause between/i);
    const failures = screen.getByLabelText(/^Auto-pause after/i);

    fireEvent.change(max, { target: { value: '5' } });
    fireEvent.blur(max);
    fireEvent.change(interval, { target: { value: '30' } });
    fireEvent.blur(interval);
    fireEvent.change(failures, { target: { value: '3' } });
    fireEvent.blur(failures);

    await waitFor(() => {
      expect(invoke).toHaveBeenLastCalledWith(
        'update_mesh_loop_config',
        expect.objectContaining({
          meshId: 42,
          mode: 'looping',
          maxIterations: 5,
          intervalSeconds: 30,
          consecutiveFailures: 3,
        })
      );
    });

    // Clearing max iterations persists `null` (continuous) rather than
    // coercing to 0 (which would trip the backend's `n >= 1` guard).
    fireEvent.change(max, { target: { value: '' } });
    fireEvent.blur(max);
    await waitFor(() => {
      expect(invoke).toHaveBeenLastCalledWith(
        'update_mesh_loop_config',
        expect.objectContaining({ maxIterations: null })
      );
    });
  });

  it('rejects invalid numeric input with a SaveIndicator error and skips the IPC', async () => {
    mockBackend(meshRow({ autopilot_mode: 'looping' }));
    openProbeDestination('autopilot');

    const max = await screen.findByLabelText(/^Max iterations/i);
    // "-1" passes type="number" coercion but fails the form's regex +
    // range check (1..). The SaveIndicator surfaces the reason; the
    // IPC is not called.
    fireEvent.change(max, { target: { value: '-1' } });
    fireEvent.blur(max);

    await waitFor(() => {
      expect(
        invoke.mock.calls.some(
          ([cmd]) => (cmd as string) === 'update_mesh_loop_config'
        )
      ).toBe(false);
    });
    expect(await screen.findByText(/Save failed/i)).toBeTruthy();
  });

  it('worktree toggle writes use_worktree via its dedicated IPC, not the loop config', async () => {
    mockBackend(meshRow({ autopilot_mode: 'looping' }));
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    const checkbox = await screen.findByLabelText(/Run loop iterations in a worktree/i);
    // Fixture use_worktree=true â†’ uncheck on click.
    await user.click(checkbox);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_use_worktree', {
        meshId: 42,
        useWorktree: false,
      });
    });
    expect(
      invoke.mock.calls.some(
        ([cmd]) => (cmd as string) === 'update_mesh_loop_config'
      )
    ).toBe(false);
  });

  it('shows Stopped with Start enabled and Stop disabled when the loop is off', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'looping',
        autopilot_enabled: false,
        loop_initial_prompt: 'ship the next issue',
      })
    );
    openProbeDestination('autopilot');

    const badge = await screen.findByTestId('loop-status-badge');
    // The badge starts as "Checking…" then resolves to Stopped once the
    // first get_loop_status returns (enabled=false, no live iteration).
    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('stopped');
    });
    expect(badge.textContent).toContain('Stopped');

    // Loop is off + a prompt is set â†’ Start is the live action, Stop is inert.
    expect(screen.getByRole('button', { name: 'Start loop' }).hasAttribute('disabled')).toBe(false);
    expect(screen.getByRole('button', { name: 'Stop loop' }).hasAttribute('disabled')).toBe(true);
    // No Pause in the Start/Stop MVP.
    expect(screen.queryByRole('button', { name: 'Pause loop' })).toBeNull();
  });

  it('Start flips autopilot_enabled on and the badge moves to Idle', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'looping',
        autopilot_enabled: false,
        loop_initial_prompt: 'ship the next issue',
      })
    );
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('stopped');
    });

    await user.click(screen.getByRole('button', { name: 'Start loop' }));

    expect(invoke).toHaveBeenCalledWith('set_mesh_autopilot_enabled', {
      meshId: 42,
      enabled: true,
    });
    // The immediate refetch reflects the flipped flag â†’ Idle (enabled, no
    // iteration running yet).
    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('idle');
    });
  });

  it('Stop flips autopilot_enabled off and the badge moves to Stopped', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'looping',
        autopilot_enabled: true,
        loop_initial_prompt: 'ship the next issue',
      })
    );
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('idle');
    });

    await user.click(screen.getByRole('button', { name: 'Stop loop' }));

    expect(invoke).toHaveBeenCalledWith('set_mesh_autopilot_enabled', {
      meshId: 42,
      enabled: false,
    });
    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('stopped');
    });
  });

  it('disables Start when the initial prompt is blank (loop would stay idle)', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'looping',
        autopilot_enabled: false,
        loop_initial_prompt: null,
      })
    );
    openProbeDestination('autopilot');

    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('stopped');
    });
    const start = screen.getByRole('button', { name: 'Start loop' });
    expect(start.hasAttribute('disabled')).toBe(true);
    expect(start.getAttribute('title')).toMatch(/initial prompt/i);
  });

  it('renders Active loop iteration N from the live status', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'looping',
        autopilot_enabled: true,
        loop_initial_prompt: 'ship the next issue',
      }),
      { loopStatus: { enabled: true, active_iteration: 3, total_iterations: 3 } }
    );
    openProbeDestination('autopilot');

    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('active');
    });
    expect(screen.getByTestId('loop-status-badge').textContent).toContain(
      'Active loop iteration 3'
    );
    // A running loop â†’ Start is inert, Stop is live.
    expect(screen.getByRole('button', { name: 'Start loop' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('button', { name: 'Stop loop' }).hasAttribute('disabled')).toBe(false);
  });

  it('surfaces a Start/Stop rejection in the SaveIndicator', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'looping',
        autopilot_enabled: false,
        loop_initial_prompt: 'ship the next issue',
      }),
      { rejectSetEnabled: true }
    );
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('stopped');
    });
    await user.click(screen.getByRole('button', { name: 'Start loop' }));

    // Start/Stop is a user-triggered write, so a rejection lands in the
    // SaveIndicator (not swallowed like the read-only status poll).
    expect(await screen.findByText(/Save failed: .*start\/stop/i)).toBeTruthy();
  });

  it('surfaces a backend rejection in the SaveIndicator', async () => {
    mockBackend(meshRow({ autopilot_mode: 'looping' }), { rejectLoopConfig: true });
    openProbeDestination('autopilot');

    const max = await screen.findByLabelText(/^Max iterations/i);
    fireEvent.change(max, { target: { value: '10' } });
    fireEvent.blur(max);

    expect(await screen.findByText(/Save failed: .*mock/i)).toBeTruthy();
  });

  it('stays a registered inspector destination so openProbeTab lands it cleanly', () => {
    // Pure routing smoke test — pinning the destination's ownership entry
    // makes a future rename that drops the PROBE_TAB_DEFINITIONS entry fail
    // with an unambiguous error.
    mockBackend(meshRow());
    openProbeDestination('autopilot');
    expect(screen.getByRole('region', { name: 'Probe panel' }).textContent).toContain('Autopilot');
  });
});

// ── Issue-driven Autopilot Policy (ticket #1013) ────────────────────────────â”€
// The four policy columns + master enable flag moved out of
// `MeshPropertiesTab` and into this tab's issue-driven branch. They
// persist atomically through `update_mesh_autopilot` (same IPC shape
// the legacy Mesh Properties used; only the call site changed). The
// tests below pin that move end-to-end: the form surface, the load
// pre-population, the save-on-change contract, and the regression
// that the same controls are NOT also rendered in Mesh Properties.
describe('AutopilotProbeTab — Issue-Driven Autopilot Policy (ticket #1013)', () => {
  // The new tab is the single configure surface for both modes. The
  // test fixture seeds a known good policy so we don't have to drive
  // the master checkbox on before every assertion.
  const ON_ROW = (): MeshRow =>
    meshRow({
      autopilot_mode: 'issue_driven',
      autopilot_enabled: true,
      autopilot_trigger_label: 'buildmesh:run',
      autopilot_concurrency_limit: 4,
      autopilot_provider: 'codex',
      autopilot_action_on_success: 'pr',
    });

  it('renders all four policy fields when autopilot is on', async () => {
    mockBackend(ON_ROW());
    openProbeDestination('autopilot');

    // The master checkbox shows enabled=true. The four policy fields
    // render because the gate is open. `Trigger label` carries the
    // "(default if blank)" hint inline in the `<label>`; RTL's
    // `getByLabelText` is exact-match by default, so the regex form
    // (anchored at the start, like the existing looping-section
    // tests' `/^Initial prompt/i`) is what survives a future hint
    // rewrite.
    expect(
      ((await screen.findByLabelText(/^Autopilot on/i)) as HTMLInputElement).checked
    ).toBe(true);
    expect(screen.getByLabelText(/^Trigger label/)).toBeTruthy();
    expect(screen.getByLabelText('Max concurrent autopilot nodes')).toBeTruthy();
    expect(screen.getByLabelText('Autopilot provider')).toBeTruthy();
    expect(screen.getByLabelText('On success')).toBeTruthy();
  });

  it('preloads the saved policy values into the form', async () => {
    mockBackend(ON_ROW(), {
      // Seed a provider catalogue so the `<select value="codex">` finds
      // a matching `<option value="codex">` — without this, the select
      // falls back to the `<Mesh default>` option (empty value) and
      // `provider.value` would read `''`, not the saved `'codex'`.
      providers: [
        { id: 'claude', label: 'Claude Code', color: '#000', icon: '', resumable: true, harness_id: 'claude', provider_id: null, is_proxied: false, group_key: 'claude' },
        { id: 'codex', label: 'Codex', color: '#000', icon: '', resumable: false, harness_id: 'codex', provider_id: null, is_proxied: false, group_key: 'codex' },
      ],
    });
    openProbeDestination('autopilot');

    const trigger = (await screen.findByLabelText(/^Trigger label/)) as HTMLInputElement;
    expect(trigger.value).toBe('buildmesh:run');
    const concurrency = screen.getByLabelText(
      'Max concurrent autopilot nodes'
    ) as HTMLSelectElement;
    expect(concurrency.value).toBe('4');
    const provider = screen.getByLabelText('Autopilot provider') as HTMLSelectElement;
    expect(provider.value).toBe('codex');
    const action = screen.getByLabelText('On success') as HTMLSelectElement;
    expect(action.value).toBe('pr');
  });

  it('hides the four policy fields when the master toggle is off (default)', async () => {
    mockBackend(meshRow({ autopilot_enabled: false }));
    openProbeDestination('autopilot');

    expect(
      ((await screen.findByLabelText(/^Autopilot on/i)) as HTMLInputElement).checked
    ).toBe(false);
    // Fields gated behind the master toggle. queryByLabelText returns
    // null without throwing — the test asserts the absence.
    expect(screen.queryByLabelText(/^Trigger label/)).toBeNull();
    expect(screen.queryByLabelText('Max concurrent autopilot nodes')).toBeNull();
    expect(screen.queryByLabelText('Autopilot provider')).toBeNull();
    expect(screen.queryByLabelText('On success')).toBeNull();
  });

  it('replaces the issue-driven form with the looping form when mode flips to Looping', async () => {
    mockBackend(ON_ROW());
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    // Sanity: issue-driven fields are visible right now.
    expect(await screen.findByLabelText(/^Trigger label/)).toBeTruthy();

    await user.click(screen.getByRole('button', { name: 'Looping' }));

    // The mode toggle writes update_mesh_loop_config carrying the
    // current prompts/caps; the issue-driven fields unmount (they
    // live inside the issue-driven branch only). The looping branch's
    // own surfaces mount.
    expect(screen.queryByLabelText(/^Trigger label/)).toBeNull();
    expect(screen.queryByLabelText('Max concurrent autopilot nodes')).toBeNull();
    expect(await screen.findByLabelText(/^Initial prompt/i)).toBeTruthy();
  });

  it('master on/off flip writes the full policy atomically via update_mesh_autopilot', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'issue_driven',
        autopilot_enabled: true,
        autopilot_trigger_label: 'buildmesh:run',
        autopilot_concurrency_limit: 4,
        autopilot_provider: 'codex',
        autopilot_action_on_success: 'pr',
      })
    );
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    const checkbox = await screen.findByLabelText(/^Autopilot on/i);
    await user.click(checkbox);

    // Atomic 5-field write (ticket #1013 — the same contract the
    // pre-#1013 Mesh Properties held): one IPC carries enabled +
    // all four policy columns so a partial-update can't leave the
    // policy out of sync with the master enable.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_autopilot', {
        meshId: 42,
        enabled: false,
        triggerLabel: 'buildmesh:run',
        concurrencyLimit: 4,
        provider: 'codex',
        actionOnSuccess: 'pr',
      });
    });
  });

  it('trigger label saves on blur via update_mesh_autopilot (atomic 5-field write)', async () => {
    mockBackend(ON_ROW());
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    const input = await screen.findByLabelText(/^Trigger label/);
    await user.clear(input);
    await user.type(input, 'buildmesh:rerun');
    fireEvent.blur(input);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'update_mesh_autopilot',
        expect.objectContaining({
          meshId: 42,
          enabled: true,
          triggerLabel: 'buildmesh:rerun',
          // Concurrency / provider / action load from MeshRow unchanged.
          concurrencyLimit: 4,
          provider: 'codex',
          actionOnSuccess: 'pr',
        })
      );
    });
  });

  it('concurrency / provider / action saves on change via the same atomic IPC', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'issue_driven',
        autopilot_enabled: true,
        autopilot_trigger_label: 'buildmesh:run',
        autopilot_concurrency_limit: 2,
        autopilot_provider: null,
        autopilot_action_on_success: 'draft_pr',
      })
    );
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    // Atomic 5-field contract — every IPC carries ALL policy columns
    // (enabled + trigger + concurrency + provider + action) so a
    // partial update can never desync the master enable from the rest
    // of the policy. Each change below asserts the FULL payload the
    // contract pins, not just the changed field. Issue #1152 added a
    // `get_autopilot_compatibility` follow-up call after every save,
    // so we assert via `toHaveBeenCalledWith` (not `Last`) — the
    // exact "last call" is now the compatibility refresh.
    // The form loads asynchronously; the first control lookup awaits it
    // (findBy*), after which the form is mounted for the rest of the test.
    await user.selectOptions(
      await screen.findByLabelText('Max concurrent autopilot nodes'),
      '5'
    );
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_autopilot', {
        meshId: 42,
        enabled: true,
        triggerLabel: 'buildmesh:run',
        concurrencyLimit: 5,
        provider: null,
        actionOnSuccess: 'draft_pr',
      });
    });

    // A blank provider value is the "<Mesh default>" sentinel; the
    // IPC carries it as `null` and the backend applies the default.
    // Untouched fields travel with the change — proving atomicity.
    await user.selectOptions(screen.getByLabelText('Autopilot provider'), '');
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_autopilot', {
        meshId: 42,
        enabled: true,
        triggerLabel: 'buildmesh:run',
        concurrencyLimit: 5,
        provider: null,
        actionOnSuccess: 'draft_pr',
      });
    });

    await user.selectOptions(screen.getByLabelText('On success'), 'none');
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('update_mesh_autopilot', {
        meshId: 42,
        enabled: true,
        triggerLabel: 'buildmesh:run',
        concurrencyLimit: 5,
        provider: null,
        actionOnSuccess: 'none',
      });
    });
  });

  it('blank trigger label serialises as null (the poller applies its default)', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'issue_driven',
        autopilot_enabled: true,
        autopilot_trigger_label: 'buildmesh:run',
      })
    );
    openProbeDestination('autopilot');

    const input = await screen.findByLabelText(/^Trigger label/);
    fireEvent.change(input, { target: { value: '   ' } });
    fireEvent.blur(input);

    // Whitespace-only collapses to null at write time so the backend
    // isn't asked to store a literal empty string. Matches the
    // pre-#1013 `saveAutopilot` contract in `MeshPropertiesTab`.
    await waitFor(() => {
      expect(invoke).toHaveBeenLastCalledWith(
        'update_mesh_autopilot',
        expect.objectContaining({ triggerLabel: null })
      );
    });
  });

  // ---------------------------------------------------------------------
  // Issue #1152 — Autopilot compatibility gate UI behaviour
  // ---------------------------------------------------------------------

  /** Helper to construct a `mockBackend` options bag with a specific
   *  compatibility verdict. The defaults elsewhere use a fully-compatible
   *  verdict so existing tests don't need to know about the gate; here
   *  every test wants a specific verdict so we centralise the helper. */
  function compatVerdict(overrides: {
    allowed?: boolean;
    reasons?: Array<Record<string, unknown>>;
    resolved_harness_id?: string | null;
    resolved_spawn_option?: string | null;
    explicit_autopilot_provider?: boolean;
  }) {
    return {
      allowed: overrides.allowed ?? false,
      reasons: overrides.reasons ?? [],
      resolved_harness_id: overrides.resolved_harness_id ?? null,
      resolved_spawn_option: overrides.resolved_spawn_option ?? null,
      explicit_autopilot_provider: overrides.explicit_autopilot_provider ?? false,
    };
  }

  it('renders the compatibility banner with the resolved spawn option + each reason headline', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'issue_driven',
        autopilot_enabled: false,
        use_worktree: true,
        autopilot_provider: 'opencode',
      }),
      {
        compatibility: compatVerdict({
          allowed: false,
          reasons: [
            { kind: 'missing_prefill', harness_id: 'opencode' },
            { kind: 'missing_attention_hook', harness_id: 'opencode' },
          ],
          resolved_harness_id: 'opencode',
          resolved_spawn_option: 'opencode',
          explicit_autopilot_provider: true,
        }),
      }
    );
    openProbeDestination('autopilot');

    // Banner is visible with the explicit-selection label.
    const banner = await screen.findByTestId('autopilot-compatibility-banner');
    expect(banner.textContent).toMatch(/Autopilot selection is incompatible/);
    expect(banner.textContent).toMatch(/resolved:.*opencode/);

    // Each reason renders as its own bullet with the harness id.
    expect(
      screen.getByTestId('autopilot-compatibility-reason-missing_prefill')
    ).toBeTruthy();
    expect(
      screen.getByTestId('autopilot-compatibility-reason-missing_attention_hook')
    ).toBeTruthy();
  });

  it('uses the default-spawn-option label when the verdict fell through', async () => {
    mockBackend(
      meshRow({ autopilot_mode: 'issue_driven', autopilot_enabled: false }),
      {
        compatibility: compatVerdict({
          allowed: false,
          reasons: [
            { kind: 'missing_prefill', harness_id: 'opencode' },
          ],
          resolved_harness_id: 'opencode',
          resolved_spawn_option: 'opencode',
          explicit_autopilot_provider: false,
        }),
      }
    );
    openProbeDestination('autopilot');

    const banner = await screen.findByTestId('autopilot-compatibility-banner');
    expect(banner.textContent).toMatch(/Default Autopilot Spawn Option is incompatible/);
  });

  it('disables the master Autopilot-on checkbox while incompatible (but keeps Stop available)', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'issue_driven',
        autopilot_enabled: false,
        autopilot_provider: 'opencode',
      }),
      {
        compatibility: compatVerdict({
          allowed: false,
          reasons: [
            { kind: 'missing_prefill', harness_id: 'opencode' },
            { kind: 'missing_attention_hook', harness_id: 'opencode' },
          ],
          resolved_harness_id: 'opencode',
          resolved_spawn_option: 'opencode',
          explicit_autopilot_provider: true,
        }),
      }
    );
    openProbeDestination('autopilot');

    const checkbox = (await screen.findByTestId(
      'autopilot-policy-enabled'
    )) as HTMLInputElement;
    expect(checkbox.disabled).toBe(true);
    expect(checkbox.title).toMatch(/cannot run on this Mesh/i);

    // The user can still see the four policy columns are gated behind the
    // master toggle — they aren't rendered until Autopilot is enabled.
    expect(screen.queryByLabelText('Trigger label')).toBeNull();
  });

  it('lets the user re-enable after switching to a compatible harness', async () => {
    // Pin: when the verdict flips from `allowed=false` to `allowed=true`,
    // the UI unblocks (banner disappears, checkbox enabled). The actual
    // IPC re-mock flow is exercised by the integration suite — here we
    // mount with the *new* allowed verdict directly and verify the UI
    // state, which is the observable outcome the user sees.
    mockBackend(
      meshRow({
        autopilot_mode: 'issue_driven',
        autopilot_enabled: false,
        autopilot_provider: 'claude:minimax',
      }),
      {
        compatibility: compatVerdict({
          allowed: true,
          resolved_harness_id: 'claude',
          resolved_spawn_option: 'claude:minimax',
          explicit_autopilot_provider: true,
        }),
      }
    );
    openProbeDestination('autopilot');
    await waitFor(() => {
      expect(screen.queryByTestId('autopilot-compatibility-banner')).toBeNull();
    });
    const checkbox = (await screen.findByTestId(
      'autopilot-policy-enabled'
    )) as HTMLInputElement;
    expect(checkbox.disabled).toBe(false);
  });

  it('disables Looping-mode Start while incompatible but keeps Stop available', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'looping',
        autopilot_enabled: false,
        loop_initial_prompt: 'do the work',
        autopilot_provider: 'opencode',
      }),
      {
        compatibility: compatVerdict({
          allowed: false,
          reasons: [
            { kind: 'missing_prefill', harness_id: 'opencode' },
            { kind: 'missing_attention_hook', harness_id: 'opencode' },
          ],
          resolved_harness_id: 'opencode',
          resolved_spawn_option: 'opencode',
          explicit_autopilot_provider: true,
        }),
      }
    );
    const user = userEvent.setup();
    openProbeDestination('autopilot');

    const start = (await screen.findByTestId(
      'autopilot-loop-start'
    )) as HTMLButtonElement;
    const stop = (await screen.findByRole('button', { name: /stop loop/i })) as HTMLButtonElement;
    expect(start.disabled).toBe(true);
    expect(start.title).toMatch(/cannot run on this Mesh/i);
    // Stop is gated on the live loop status (Idle/Active), not the
    // compatibility verdict — so it's *not* disabled purely by an
    // incompatible Spawn Option. The user can always recover.
    expect(stop).toBeTruthy();
    // Suppress unused warning.
    void user;
  });

  it('shows the worktree-disabled reason when the mesh has worktrees off', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'issue_driven',
        autopilot_enabled: false,
        use_worktree: false,
      }),
      {
        compatibility: compatVerdict({
          allowed: false,
          reasons: [{ kind: 'worktree_disabled' }],
          resolved_harness_id: 'claude',
          resolved_spawn_option: 'claude',
          explicit_autopilot_provider: false,
        }),
      }
    );
    openProbeDestination('autopilot');

    const banner = await screen.findByTestId('autopilot-compatibility-banner');
    expect(banner.textContent).toMatch(/worktrees are disabled on this mesh/i);
  });

  it('does not render the banner when the verdict is allowed', async () => {
    mockBackend(
      meshRow({
        autopilot_mode: 'issue_driven',
        autopilot_enabled: false,
      }),
      {
        compatibility: compatVerdict({
          allowed: true,
          resolved_harness_id: 'claude',
          resolved_spawn_option: 'claude',
        }),
      }
    );
    openProbeDestination('autopilot');
    // Give the verdict fetch a tick to resolve.
    await waitFor(() => {
      expect(screen.queryByTestId('autopilot-compatibility-banner')).toBeNull();
    });
  });
});
