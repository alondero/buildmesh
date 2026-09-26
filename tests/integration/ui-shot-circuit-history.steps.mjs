import { expect } from '@playwright/test';
import { DatabaseSync } from 'node:sqlite';
import path from 'node:path';

// Dev-profile fixture only (#1909). Seeds durable Circuit Run History rows for
// wait / capacity / configuration / recovery so the real backend IPC
// (`circuit_run_history`) and the rendered Probe can be checked at 240px. No
// agents or enabled triggers are created.
export default async function ({ page, invoke }) {
  const mesh = await invoke('create_test_mesh', { name: 'Circuit history verification' });
  const db = new DatabaseSync(path.join(process.env.APPDATA, 'com.alond.buildmesh.dev', 'buildmesh.db'));
  try {
    db.prepare('UPDATE meshes SET path=?, pre_spawn_pool_size=0 WHERE id=?')
      .run(process.cwd(), mesh.id);
    const graph = JSON.stringify({ version: 2, nodes: [], edges: [] });
    const circuit = db.prepare('INSERT INTO autopilot_circuits (mesh_id,name,graph_json,enabled) VALUES (?,?,?,0)')
      .run(mesh.id, 'History 240px verification', graph).lastInsertRowid;
    const run = db.prepare("INSERT INTO autopilot_circuit_runs (circuit_id,mesh_id,trigger_identity,state,context_json) VALUES (?,?,'manual:history-240','failed','{}')")
      .run(circuit, mesh.id).lastInsertRowid;
    db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt,outcome,error_message) VALUES (?,?, 'failed',1,'failed','Evidence window ended without a readable report.')")
      .run(run, 'reviewer');
    // One entry per causal kind, each with source/disposition and identity.
    const history = db.prepare('INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail,source,disposition) VALUES (?,?,?,?,?,?,?)');
    const pinned = history.run(run, null, null, 'configuration_pinned',
      JSON.stringify({ behavior_revision: 1, graph_sha256: 'abcdef0123456789', reviewers: [{ node_id: 'reviewer' }] }),
      'run.configuration', 'applied').lastInsertRowid;
    const queueWait = history.run(run, null, null, 'queue_wait',
      JSON.stringify({ reason: 'mesh_capacity', capacity: 2 }),
      'circuit_worker.admission', 'waiting').lastInsertRowid;
    history.run(run, 'spawn', 1, 'step_capacity_wait',
      JSON.stringify({ before: null, after: JSON.stringify({ circuit_limit: true, agent_limit: false }) }),
      'circuit_worker.capacity', 'waiting');
    history.run(run, 'reviewer', 1, 'evidence_window_changed',
      JSON.stringify({ before: null, after: { attempt: '1', timeout_ms: '60000', since_ms: '1767225600000', observed: '0', explicit_budget: '0' } }),
      'circuit_worker.reconciliation', 'waiting');
    history.run(run, 'reviewer', 1, 'operator_attestation',
      'Operator-recorded outcome (NotPerformed): Confirmed request was rejected before dispatch',
      'operator', 'not_performed');
    const recovery = history.run(run, null, null, 'recovery',
      JSON.stringify({ successor_run_id: 88, rounds: 2 }),
      'operator', 'applied').lastInsertRowid;

    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator(`#mesh-item-name-${mesh.id}`).click();
    await page.getByRole('button', { name: 'Search or open' }).click();
    await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
    await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await page.keyboard.press('End');
    await page.getByTestId('circuits-view-history').click();
    // A failed run is default-expanded; open its Circuit Run History.
    await expect(page.getByTestId(`run-toggle-${run}`)).toHaveAttribute('aria-expanded', 'true');
    await page.getByText('Circuit Run History').click();

    // Structured, human-readable detail with identity and provenance.
    await expect(page.getByTestId(`history-entry-${queueWait}`)).toBeVisible();
    await expect(page.getByTestId(`history-entry-${pinned}`)).toBeVisible();
    await expect(page.getByTestId(`history-entry-${recovery}`)).toBeVisible();
    await expect(page.getByText(/all 2 circuit-run slot/)).toBeVisible();
    await expect(page.getByText(/Step capacity wait — step slots busy · agent slot free/)).toBeVisible();
    await expect(page.getByText(/Recovered into run #88/)).toBeVisible();
    await expect(page.getByText('Source: circuit_worker.admission · waiting')).toBeVisible();
    await expect(page.getByTestId(`history-entry-${queueWait}`)).toHaveAttribute('data-disposition', 'waiting');
    await expect(page.getByTestId(`history-entry-${recovery}`)).toHaveAttribute('data-disposition', 'applied');

    // 240px acceptance: the panel is at its minimum width and neither the tab
    // nor the history body escapes sideways or clips a control.
    const bounds = await page.getByTestId('circuits-probe-tab').evaluate((root) => ({
      width: root.getBoundingClientRect().width, scroll: root.scrollWidth, client: root.clientWidth,
    }));
    expect(bounds.width).toBeLessThanOrEqual(240);
    expect(bounds.scroll).toBeLessThanOrEqual(bounds.client);
    const body = await page.getByTestId('circuits-probe-body').evaluate((node) => ({
      scroll: node.scrollWidth, client: node.clientWidth,
    }));
    expect(body.scroll).toBeLessThanOrEqual(body.client);
    const clipped = await page.getByTestId('circuits-probe-tab').evaluate((root) => {
      const rect = root.getBoundingClientRect();
      return [...root.querySelectorAll('button, select, input')].some((control) => {
        const box = control.getBoundingClientRect();
        return box.width > 0 && (box.left < rect.left || box.right > rect.right);
      });
    });
    expect(clipped).toBe(false);
    console.log(`CIRCUIT_HISTORY_FIXTURE_MESH=${mesh.id} RUN=${run}`);
  } finally {
    db.close();
    // The fixture creates durable circuit rows directly; remove the mesh after
    // the DB handle is closed (mesh deletion cascades circuit/run/history).
    try {
      await invoke('delete_mesh', { meshId: mesh.id });
    } catch (error) {
      console.warn(`Could not clean up circuit history fixture mesh ${mesh.id}:`, error);
    }
  }
}
