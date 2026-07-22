/**
 * Tests for the Autopilot Probe tab — wayfinder #990, ticket #994.
 *
 * Strategy mirrors `mesh-properties-tab.test.tsx`: mount the full
 * `ProbePanel`, click the new tab's activity-rail button, then assert
 * on the rendered form and on the IPC calls the tab fires. The mesh
 * store is seeded directly, the `invoke` mock is wired per-test, and
 * the `MeshRow` fixture carries the full v30 surface (issue-driven
 * columns + the six looping columns) so the tab's load effect sees
 * a realistic row.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { MeshRow } from '../../src/types/generated/MeshRow';
import type { LoopStatusDto } from '../../src/types/generated/LoopStatus';

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

async function openAutopilotTab() {
  const user = userEvent.setup();
  render(<ProbePanel />);
  // Activity-rail button — tooltip/label both resolve to "Autopilot".
  await user.click(screen.getByRole('button', { name: 'Autopilot' }));
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
});

describe('AutopilotProbeTab (wayfinder #990 ticket #994)', () => {
  it('mounts via the activity rail and shows the mode toggle (issue-driven by default)', async () => {
    mockBackend(meshRow());
    await openAutopilotTab();

    // The segmented control is always visible (both modes exist). The
    // ticket's label "Autopilot" is on the rail button AND in the
    // header, so the role lookup for the button is unambiguous.
    expect(await screen.findByRole('button', { name: 'Autopilot' })).toBeTruthy();
    expect(
      (await screen.findByRole('button', { name: 'Looping', pressed: false }))
    ).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Issue-Driven' }).getAttribute('aria-pressed')).toBe('true');

    // In issue-driven mode the looping inputs are hidden and the
    // pointer-to-Mesh-Properties paragraph is in the DOM. We pin a
    // phrase unique to that paragraph (`Switching this toggle to
    // Looping is non-destructive`) because the title "Mesh
    // Properties" appears in both the issue-driven body and the
    // activity rail's accessible name.
    expect(screen.queryByLabelText(/^Initial prompt/i)).toBeNull();
    expect(screen.queryByTestId('loop-status-badge')).toBeNull();
    expect(
      screen.getByText(/Switching this toggle to Looping is non-destructive/i)
    ).toBeTruthy();
  });

  it('switching to Looping writes the full loop config atomically', async () => {
    mockBackend(meshRow());
    const user = userEvent.setup();
    await openAutopilotTab();

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
    await openAutopilotTab();

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
    const user = userEvent.setup();
    await openAutopilotTab();

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
    const user = userEvent.setup();
    await openAutopilotTab();

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
    await openAutopilotTab();

    const checkbox = await screen.findByLabelText(/Run loop iterations in a worktree/i);
    // Fixture use_worktree=true → uncheck on click.
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
    await openAutopilotTab();

    const badge = await screen.findByTestId('loop-status-badge');
    // The badge starts as "Checking…" then resolves to Stopped once the
    // first get_loop_status returns (enabled=false, no live iteration).
    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('stopped');
    });
    expect(badge.textContent).toContain('Stopped');

    // Loop is off + a prompt is set → Start is the live action, Stop is inert.
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
    await openAutopilotTab();

    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('stopped');
    });

    await user.click(screen.getByRole('button', { name: 'Start loop' }));

    expect(invoke).toHaveBeenCalledWith('set_mesh_autopilot_enabled', {
      meshId: 42,
      enabled: true,
    });
    // The immediate refetch reflects the flipped flag → Idle (enabled, no
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
    await openAutopilotTab();

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
    await openAutopilotTab();

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
    await openAutopilotTab();

    await waitFor(() => {
      expect(screen.getByTestId('loop-status-badge').getAttribute('data-status')).toBe('active');
    });
    expect(screen.getByTestId('loop-status-badge').textContent).toContain(
      'Active loop iteration 3'
    );
    // A running loop → Start is inert, Stop is live.
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
    await openAutopilotTab();

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
    const user = userEvent.setup();
    await openAutopilotTab();

    const max = await screen.findByLabelText(/^Max iterations/i);
    fireEvent.change(max, { target: { value: '10' } });
    fireEvent.blur(max);

    expect(await screen.findByText(/Save failed: .*mock/i)).toBeTruthy();
  });

  it('registers its rail button so the activity bar lands it cleanly', () => {
    // Pure routing smoke test — the rest of the suite opens via the
    // rail, but pinning "the rail exposes the label" here makes a
    // future rename that drops a PROBE_TABS entry fail with an
    // unambiguous error.
    mockBackend(meshRow());
    render(<ProbePanel />);
    expect(screen.getByRole('button', { name: 'Autopilot' })).toBeTruthy();
  });
});
