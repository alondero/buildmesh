import { expect } from '@playwright/test';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';
import { mkdirSync } from 'node:fs';

// Real dev backend and WebView2 with completed fixture rows: no agent spawns.
// Rust tests cover worker parent resolution; this covers durable ownership UI.
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
    db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id, node_id, agent_node_id, parent_agent_node_id, status) VALUES (?, 'reviewer', ?, ?, 'completed')").run(run, reviewer, source);
    for (const scope of ['node', 'autopilot']) {
      if (scope === 'autopilot') {
        db.prepare('UPDATE autopilot_circuit_runs SET source_agent_node_id=NULL WHERE id=?').run(run);
        db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id, node_id, agent_node_id, status) VALUES (?, 'implementer', ?, 'completed')").run(run, source);
      }
      await page.reload({ waitUntil: 'domcontentloaded' });
      await page.locator(`#mesh-item-name-${mesh.id}`).click();
      const implementation = page.locator(`#activity-${source}-agent-${source}`);
      const review = page.locator(`#activity-${source}-agent-${reviewer}`);
      await expect(implementation).toBeVisible();
      await expect(review).toBeVisible();
      await review.click();
      await expect(review).toHaveAttribute('aria-selected', 'true');
      await implementation.click();
      await expect(implementation).toHaveAttribute('aria-selected', 'true');
      const card = await page.locator(`#activity-panel-${source}`).locator('..').boundingBox();
      if (!card) throw new Error('Review node card is not rendered');
      await page.screenshot({ path: `${output}/${scope}-review-activities.png`,
        clip: { ...card, height: Math.min(card.height, 110) } });
    }
    const launch = page.locator(`#activity-panel-${source}`).locator('..').getByRole('button', { name: 'Start the Review Loop or Circuit', exact: true });
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
