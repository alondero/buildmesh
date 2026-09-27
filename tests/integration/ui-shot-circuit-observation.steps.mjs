import { expect } from '@playwright/test';
import { DatabaseSync } from 'node:sqlite';
import path from 'node:path';

// Render/read-path verification only: history is seeded directly with SQL.
// This does not exercise worker -> ledger writes or autonomous harness progress.

export default async function ({ page, invoke }) {
  const mesh = await invoke('create_test_mesh', { name: 'Session observation verification' });
  const db = new DatabaseSync(path.join(process.env.APPDATA, 'com.alond.buildmesh.dev', 'buildmesh.db'));
  try {
    db.prepare('UPDATE meshes SET path=?, pre_spawn_pool_size=0 WHERE id=?').run(process.cwd(), mesh.id);
    const graph = JSON.stringify({ version: 2, nodes: [{ id: 'review', type: { type: 'llm_turn_classifier', target_node_id: null } }], edges: [] });
    const circuit = db.prepare('INSERT INTO autopilot_circuits (mesh_id,name,graph_json,enabled,concurrency_limit) VALUES (?,?,?,0,1)').run(mesh.id, 'Observation readiness', graph).lastInsertRowid;
    const run = db.prepare('INSERT INTO autopilot_circuit_runs (circuit_id,mesh_id,trigger_identity,state,context_json) VALUES (?,?,?,?,?)').run(circuit, mesh.id, 'manual:observation-ui', 'paused', '{}').lastInsertRowid;
    const message = 'The session has an unfinished input draft. Submit or clear it; Buildmesh will recheck automatically.';
    db.prepare('INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt,error_message) VALUES (?,?,?,?,?)').run(run, 'review', 'unverified', 1, message);
    const history = db.prepare('INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail,source,disposition) VALUES (?,?,?,?,?,?,?)');
    history.run(run, 'review', 1, 'observation_readiness', JSON.stringify({ blocker: { kind: 'input_draft' }, message }), 'circuit_worker.reconciliation', 'waiting');
    history.run(run, 'review', 1, 'observation_readiness', JSON.stringify({ blocker: null, message: null }), 'circuit_worker.reconciliation', 'resolved');
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator(`#mesh-item-name-${mesh.id}`).click();
    await page.getByRole('button', { name: 'Search or open' }).click();
    await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
    await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await page.keyboard.press('End');
    const card = page.getByTestId(`run-card-${run}`);
    const toggle = page.getByTestId(`run-toggle-${run}`);
    if ((await toggle.getAttribute('aria-expanded')) !== 'true') await toggle.click();
    await card.getByText('Circuit Run History').click();
    await expect(card.getByText('Session observation', { exact: true })).toHaveCount(2);
    await expect(card.getByText(message, { exact: true }).last()).toBeVisible();
    await expect(card.getByText('Observation blocker cleared; the current report can be checked.', { exact: true })).toBeVisible();
    const panel = page.getByTestId('circuits-probe-tab');
    const bounds = await panel.evaluate(root => ({ width: root.getBoundingClientRect().width, scroll: root.scrollWidth, client: root.clientWidth }));
    expect(bounds.width).toBeLessThanOrEqual(240);
    expect(bounds.scroll).toBeLessThanOrEqual(bounds.client);
    await panel.screenshot({ path: 'docs/pr-screenshots/select-even-hen/session-observation-after.png' });
    console.log('PASS: real backend history renders blocker and recovery at 240px');
  } finally {
    db.close();
    await invoke('delete_mesh', { meshId: mesh.id });
  }
}
