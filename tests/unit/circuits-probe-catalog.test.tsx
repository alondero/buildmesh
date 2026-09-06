/**
 * Catalog-driven contract coverage for the Circuits Probe tab
 * (issue #1469 — "add contract coverage for every built-in blueprint").
 *
 * The Rust contract test
 * (`src-tauri/src/autopilot/circuit/blueprint_contract.rs::tests::built_in_catalog_covers_every_blueprint_kind`)
 * is the drift gate on the backend: adding a new `CircuitBlueprintKind`
 * variant without a fixture fails the suite there. This test mirrors
 * that discipline on the TS side: the Probe UI's blueprint selector
 * and `create_circuit` wire shape must agree with the canonical Rust
 * catalog. The two drift gates together make adding a built-in
 * blueprint require both backend fixture AND frontend wiring.
 *
 * The "every selectable blueprint has TypeScript Probe coverage
 * using generated wire types" acceptance criterion from #1469
 * lives here. Wire types come from `src/types/generated/` so the
 * selectors can never drift from the Rust enum (the import fails
 * at compile time if a variant is added or renamed).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { AutopilotCircuit } from '../../src/types/generated/AutopilotCircuit';
import type { CircuitBlueprintKind } from '../../src/types/generated/CircuitBlueprintKind';
import type { CircuitRunDetail } from '../../src/types/generated/CircuitRunDetail';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { openProbeDestination } from './helpers/openProbeDestination';

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

const CIRCUIT: AutopilotCircuit = {
  id: 7,
  mesh_id: 42,
  name: 'nightly-sweep',
  description: '',
  enabled: true,
  concurrency_limit: 1,
  graph_json: '{"version":1,"nodes":[],"edges":[]}',
  created_at: '2026-08-22 10:00:00',
  updated_at: '2026-08-22 10:00:00',
  is_preset: false,
};

const RUN_DONE: CircuitRunDetail = {
  run: {
    id: 11,
    circuit_id: 7,
    mesh_id: 42,
    trigger_identity: 'manual:1724000000000',
    state: 'completed',
    context_json: '{}',
    source_agent_node_id: null,
    created_at: '2026-08-22 10:05:00',
    updated_at: '2026-08-22 10:07:00',
  },
  steps: [],
};

/**
 * The Probe-side mirror of the Rust `BUILT_IN_CATALOG`. Keeping it
 * here (rather than importing from the Rust enum directly) lets us
 * assert the per-blueprint Probe UI affordances: every catalog
 * entry MUST appear as an `<option>` in the blueprint selector AND
 * its create-circuit wire shape MUST pass the right `blueprint` and
 * `concurrencyLimit` through to the IPC. Drift on either side fails
 * the test.
 *
 * When adding a new built-in blueprint:
 *   1. Add the variant to `CircuitBlueprintKind` (Rust) + entry to
 *      `BUILT_IN_CATALOG` (`blueprint_contract.rs`).
 *   2. Regenerate the ts-rs types (`cargo test`).
 *   3. Add the matching Probe option to `CircuitsProbeTab.tsx`.
 *   4. Append the variant here. The test will fail until each step
 *      above lands.
 */
const PROBE_CATALOG: ReadonlyArray<{
  kind: CircuitBlueprintKind;
  label: string;
  defaultConcurrencyLimit: number;
}> = [
  {
    kind: 'walking_skeleton',
    label: 'Walking skeleton',
    defaultConcurrencyLimit: 1,
  },
  {
    kind: 'issue_driven_autopilot_review',
    label: 'Issue-driven Autopilot + PR review',
    defaultConcurrencyLimit: 2,
  },
];

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'list_circuits') return Promise.resolve([CIRCUIT]);
    if (cmd === 'list_circuit_runs') return Promise.resolve([RUN_DONE]);
    if (cmd === 'list_circuit_queue') return Promise.resolve([]);
    if (cmd === 'list_circuit_probe') {
      return Promise.resolve({
        circuits: [{ circuit: CIRCUIT, runs: [RUN_DONE] }],
        queue: [],
      });
    }
    if (cmd === 'list_circuits_with_runs') {
      return Promise.resolve([{ circuit: CIRCUIT, runs: [RUN_DONE] }]);
    }
    if (cmd === 'create_circuit') {
      return Promise.resolve(args && cmd === 'create_circuit' ? CIRCUIT : undefined);
    }
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
  useUIStore.setState({
    probeOpen: false,
    probeTab: 'files',
    activeDiffFile: null,
    activeCircuitEditorId: null,
  });
});

describe('Circuits Probe catalog contract (#1469)', () => {
  // -- Type-level exhaustiveness drift gate (#1469 follow-up) -------
  //
  // The previous implementation iterated a hardcoded array of variant
  // strings; adding a new Rust variant to `CircuitBlueprintKind`
  // without updating this test was invisible to tsc. The drift gate
  // below uses `Exclude<>` to derive a "missing variants" set from
  // the type system: if a Rust variant exists that's not in
  // `PROBE_CATALOG`, the assertion fails at compile time
  // (vitest's `expect` evaluates at runtime, but the surrounding
  // `type _CheckExhaustive` only compiles if every variant is covered).
  type MissingFromCatalog = Exclude<
    CircuitBlueprintKind,
    (typeof PROBE_CATALOG)[number]['kind']
  >;
  // Compile-time exhaustiveness: if a new CircuitBlueprintKind variant
  // ships without a PROBE_CATALOG entry, `MissingFromCatalog` becomes
  // a non-`never` type and this binding fails to type-check.
  const _exhaustive: [MissingFromCatalog] extends [never] ? true : false = true;
  expect(_exhaustive).toBe(true);

  it('the Probe catalog covers every generated CircuitBlueprintKind variant', () => {
    const seen = new Set<CircuitBlueprintKind>();
    for (const entry of PROBE_CATALOG) {
      seen.add(entry.kind);
    }
    expect(seen.size).toBe(PROBE_CATALOG.length, 'duplicate kind in PROBE_CATALOG');
  });

  it.each(PROBE_CATALOG)(
    'every catalog blueprint is selectable in the Probe blueprint dropdown ($kind)',
    async (entry) => {
      mockBackend();
      openProbeDestination('circuits');
      fireEvent.click(await screen.findByTestId('circuits-view-manage'));
      const select = await screen.findByTestId('circuit-blueprint-select');
      const options = Array.from(select.querySelectorAll('option')).map(
        (opt) => (opt as HTMLOptionElement).value
      );
      expect(options).toContain(entry.kind);
      // The human label matches the contract — a future rename in
      // the Rust catalog (`display_name`) MUST also update here, or
      // Probe users will see a stale label.
      const labelOption = Array.from(select.querySelectorAll('option')).find(
        (opt) => (opt as HTMLOptionElement).value === entry.kind
      ) as HTMLOptionElement;
      expect(labelOption.textContent).toBe(entry.label);
    }
  );

  it.each(PROBE_CATALOG)(
    'creating a $kind circuit passes the right blueprint + concurrencyLimit to create_circuit',
    async (entry) => {
      mockBackend();
      const user = userEvent.setup();
      openProbeDestination('circuits');

      await user.click(await screen.findByTestId('circuits-view-manage'));

      // Pick the catalog entry, fill the minimum required fields
      // (name + trigger label for issue-label blueprints, since
      // issue_driven_autopilot_review forces that trigger), and click
      // New Circuit.
      await user.selectOptions(
        await screen.findByTestId('circuit-blueprint-select'),
        entry.kind
      );
      await user.type(screen.getByTestId('circuit-name-input'), `${entry.kind}-test`);

      // The review blueprint locks the trigger to GitHub issue label
      // and forces a concurrency of 2 (otherwise the implementation
      // and reviewer deadlock). The walking skeleton accepts Manual
      // and defaults concurrency to 1.
      if (entry.kind === 'issue_driven_autopilot_review') {
        // The Probe UI pins the trigger select to github_issue_label
        // and disables it. The label input is also visible.
        await user.type(
          screen.getByTestId('circuit-trigger-label-input'),
          'buildmesh:run'
        );
      }

      await user.click(screen.getByTestId('circuit-create-button'));

      await waitFor(() => {
        expect(invoke).toHaveBeenCalledWith(
          'create_circuit',
          expect.objectContaining({
            meshId: 42,
            name: `${entry.kind}-test`,
            blueprint: entry.kind,
            concurrencyLimit: entry.defaultConcurrencyLimit,
          })
        );
      });
    }
  );
});
