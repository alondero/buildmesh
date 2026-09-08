import { expect } from '@playwright/test';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';
import { mkdirSync } from 'node:fs';

// Real dev backend and WebView2 with completed fixture rows: no agent spawns.
// The worker attachment seam covers parent resolution; this covers ownership UI.
export default async function ({ page, invoke }) {
  const db = new DatabaseSync(join(process.env.APPDATA, 'com.alond.buildmesh.dev', 'buildmesh.db'));
  db.exec('PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;');
  const mesh = await invoke('create_test_mesh', { name: 'Review consistency verification' });
  const output = 'docs/pr-screenshots/hopeful-lifeless-epic';
  mkdirSync(output, { recursive: true });
  try {
    const addNode = db.prepare("INSERT INTO agent_nodes (mesh_id, name, path, branch, env, provider, status, use_worktree) VALUES (?, ?, ?, 'main', 'windows', 'codex', 'completed', 0)");
    const source = Number(addNode.run(mesh.id, 'Review implementation', mesh.path).lastInsertRowid);
    const reviewer = Number(addNode.run(mesh.id, 'Code reviewer', mesh.path).lastInsertRowid);
    const graph = JSON.stringify({ version: 2, nodes: [{ id: 'trigger', type: { type: 'manual' } }], edges: [] });
    const circuit = Number(db.prepare("INSERT INTO autopilot_circuits (mesh_id, name, graph_json, enabled) VALUES (?, 'Review verification', ?, 0)").run(mesh.id, graph).lastInsertRowid);
    const run = Number(db.prepare("INSERT INTO autopilot_circuit_runs (circuit_id, mesh_id, state, source_agent_node_id) VALUES (?, ?, 'completed', ?)").run(circuit, mesh.id, source).lastInsertRowid);
    // This smoke test owns only the activity-panel fixture. Reviewer parentage
    // is produced by the worker and is covered by the worker-level test; do
    // not manufacture that relationship here and accidentally paper over the
    // spawn path under test.
    db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id, node_id, agent_node_id, status) VALUES (?, 'reviewer', ?, 'completed')").run(run, reviewer);
    for (const scope of ['node', 'autopilot']) {
      if (scope === 'autopilot') {
        db.prepare('UPDATE autopilot_circuit_runs SET source_agent_node_id=NULL WHERE id=?').run(run);
        db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id, node_id, agent_node_id, status) VALUES (?, 'implementer', ?, 'completed')").run(run, source);
      }
      await page.reload({ waitUntil: 'domcontentloaded' });
      const meshItem = page.locator(`#mesh-item-name-${mesh.id}`);
      await meshItem.waitFor({ state: 'visible', timeout: 10000 });
      await meshItem.click();
      // The fixture deliberately does not manufacture parent_agent_node_id;
      // the worker test exercises that production relationship. Here we
      // verify both durable owners render without conflating their cards.
      const implementationPanel = page.locator(`#activity-panel-${source}`);
      const reviewPanel = page.locator(`#activity-panel-${reviewer}`);
      await expect(implementationPanel).toBeVisible();
      await expect(reviewPanel).toBeVisible();
      const card = await page.locator(`#activity-panel-${source}`).locator('..').boundingBox();
      if (!card) throw new Error('Review node card is not rendered');
      await page.screenshot({ path: `${output}/${scope}-review-activities.png`,
        clip: { ...card, height: Math.min(card.height, 110) } });
    }
    const launch = page.locator(`#activity-panel-${source}`).locator('..').getByRole('button', { name: 'Start review or circuit', exact: true });
    await launch.click();
    await expect(page.getByLabel('Workflow')).toHaveValue('');
    const rounds = page.getByLabel('Maximum review rounds');
    await expect(rounds).toHaveValue('3');
    await rounds.fill('0');
    await expect(page.getByRole('button', { name: 'Start review', exact: true })).toBeDisabled();
    await rounds.fill('3');
    await expect(page.getByRole('button', { name: 'Start review', exact: true })).toBeEnabled();
    await page.getByRole('dialog').screenshot({ path: `${output}/review-dialog-after.png` });
    await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    await expect(page.getByRole('dialog')).toHaveCount(0);
  } finally {
    // Only this check's fixture rows are deleted; they have no process/worktree.
    db.prepare('DELETE FROM autopilot_circuit_run_steps WHERE run_id IN (SELECT id FROM autopilot_circuit_runs WHERE mesh_id=?)').run(mesh.id);
    db.prepare('DELETE FROM autopilot_circuit_runs WHERE mesh_id=?').run(mesh.id);
    db.prepare('DELETE FROM autopilot_circuits WHERE mesh_id=?').run(mesh.id);
    db.prepare('DELETE FROM agent_nodes WHERE mesh_id=?').run(mesh.id);
    db.prepare('DELETE FROM meshes WHERE id=?').run(mesh.id);
    db.close();
    await page.reload({ waitUntil: 'domcontentloaded' });
  }
}
