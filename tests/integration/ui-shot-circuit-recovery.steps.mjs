import { expect } from '@playwright/test';
import { DatabaseSync } from 'node:sqlite';
import path from 'node:path';

// Dev-profile fixtures only. No agents or enabled triggers are created.
export default async function ({ page, invoke }) {
  const mesh = await invoke('create_test_mesh', { name: 'Circuit recovery verification' });
  const db = new DatabaseSync(path.join(process.env.APPDATA, 'com.alond.buildmesh.dev', 'buildmesh.db'));
  try {
    db.prepare('UPDATE meshes SET path=?, pre_spawn_pool_size=0 WHERE id=?')
      .run(process.cwd(), mesh.id);
    const graph = JSON.stringify({
      version: 2,
      nodes: [
        { id: 'review_classifier', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
        { id: 'review_retry', type: { type: 'retry_limit', max_retries: 3 } },
      ],
      edges: [],
    });
    const circuit = db.prepare("INSERT INTO autopilot_circuits (mesh_id,name,graph_json,enabled) VALUES (?,?,?,0)")
      .run(mesh.id, 'Review status verification', graph).lastInsertRowid;
    const run = db.prepare("INSERT INTO autopilot_circuit_runs (circuit_id,mesh_id,trigger_identity,state,context_json) VALUES (?,?,'manual:review-recovery','completed','{}')")
      .run(circuit, mesh.id).lastInsertRowid;
    const insertStep = db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt,outcome) VALUES (?,?,'completed',3,?)");
    insertStep.run(run, 'review_classifier', 'completed');
    insertStep.run(run, 'review_retry', 'failed');
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator(`#mesh-item-name-${mesh.id}`).click();
    await page.getByRole('button', { name: 'Search or open' }).click();
    await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
    await page.getByRole('option', { name: /Open Circuits/ }).click();
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await page.keyboard.press('End');
    if (process.env.CIRCUIT_SHOT_BASELINE === '1') {
      await page.getByTestId('circuits-view-history').click();
      await expect(page.getByTestId(`run-state-${run}`)).toHaveText('Completed');
    } else {
      await expect(page.getByTestId('circuits-view-activity')).toHaveAttribute('aria-selected', 'true');
      await expect(page.getByTestId(`run-state-${run}`)).toHaveText('Review limit reached');
      await expect(page.getByText(/No final approval is recorded/)).toBeVisible();
      await page.getByTestId(`run-toggle-${run}`).click();
      await expect(page.locator(`#run-detail-${run}`)).toBeVisible();
    }
    const bounds = await page.getByTestId('circuits-probe-tab').evaluate((root) => ({
      width: root.getBoundingClientRect().width, scroll: root.scrollWidth, client: root.clientWidth,
    }));
    expect(bounds.width).toBeLessThanOrEqual(240);
    expect(bounds.scroll).toBeLessThanOrEqual(bounds.client);
    console.log(`CIRCUIT_RECOVERY_FIXTURE_MESH=${mesh.id}`);
  } finally {
    db.close();
    // The fixture creates durable circuit rows directly so the screenshot can
    // exercise historical recovery. Remove the mesh after the DB handle is
    // closed; mesh deletion cascades the circuit, run, and step rows.
    try {
      await invoke('delete_mesh', { meshId: mesh.id });
    } catch (error) {
      console.warn(`Could not clean up circuit recovery fixture mesh ${mesh.id}:`, error);
    }
  }
}
