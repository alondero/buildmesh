import { expect } from '@playwright/test';
import { DatabaseSync } from 'node:sqlite';
import fs from 'node:fs';
import path from 'node:path';

// The fixture supplies an uncertain handoff. Recovery uses real Tauri IPC,
// the durable outcome command, and the ordinary worker tick to finish the run.
export default async function ({ page, invoke }) {
  const mesh = await invoke('create_test_mesh', { name: 'Circuit handoff recovery' });
  const profile = process.env.BUILDMESH_UI_PROFILE ?? 'com.alond.buildmesh.dev';
  const db = new DatabaseSync(path.join(process.env.APPDATA, profile, 'buildmesh.db'));
  const out = 'docs/pr-screenshots/mature-feline-mote';
  fs.mkdirSync(out, { recursive: true });
  try {
    db.prepare('UPDATE meshes SET path=?, pre_spawn_pool_size=0 WHERE id=?').run(process.cwd(), mesh.id);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator(`#mesh-item-name-${mesh.id}`).click();
    // Keep the synthetic, process-less agent out of the terminal canvas.
    await page.getByRole('button', { name: 'Pinned', exact: true }).click();
    const agent = Number(db.prepare("INSERT INTO agent_nodes(mesh_id,name,path,status,session_started_at) VALUES(?,?,?,'completed',1)")
      .run(mesh.id, 'Inspected source', process.cwd()).lastInsertRowid);
    const graph = JSON.stringify({ version: 2, nodes: [
      { id: 'trigger', type: { type: 'manual' } },
      { id: 'await_source', type: { type: 'await_agent_turn', target_node_id: '$source' } },
      { id: 'done', type: { type: 'notify', message: 'Recovered handoff completed' } },
    ], edges: [
      { from: 'trigger', to: 'await_source', condition: 'always' },
      { from: 'await_source', to: 'done', condition: 'always' },
    ] });
    const circuit = Number(db.prepare('INSERT INTO autopilot_circuits(mesh_id,name,graph_json,enabled,concurrency_limit) VALUES(?,?,?,0,1)')
      .run(mesh.id, 'Recover an inspected handoff', graph).lastInsertRowid);
    const context = JSON.stringify({ 'source.agent_id': String(agent) });
    const run = Number(db.prepare("INSERT INTO autopilot_circuit_runs(circuit_id,mesh_id,trigger_identity,state,context_json,source_agent_node_id) VALUES(?,?,'manual:handoff-ui','running',?,?)")
      .run(circuit, mesh.id, context, agent).lastInsertRowid);
    db.prepare("INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,attempt,outcome) VALUES(?,'trigger','completed',1,'completed')").run(run);
    db.prepare("INSERT INTO autopilot_circuit_run_steps(run_id,node_id,status,attempt,error_message) VALUES(?,'await_source','unverified',1,?)")
      .run(run, 'Evidence window ended. Inspect the source before advancing.');
    db.prepare("INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail,source,disposition) VALUES(?,'await_source',1,'checkpoint_reason',?,'circuit_worker','waiting')")
      .run(run, 'Evidence window ended. Inspect the source before advancing.');
    await page.getByRole('button', { name: 'Search or open' }).click();
    await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
    await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await page.keyboard.press('End');
    const card = page.getByTestId(`run-card-${run}`);
    const toggle = page.getByTestId(`run-toggle-${run}`);
    if (await toggle.getAttribute('aria-expanded') !== 'true') await toggle.click();
    await card.getByText('Circuit Run History', { exact: true }).click();
    const complete = card.getByRole('button', { name: 'Record completed', exact: true });
    await expect(complete).toBeDisabled();
    await card.getByLabel('Reason and supporting evidence').fill('Inspected the finished source work; continue this handoff to the next step.');
    await expect(complete).toBeEnabled();
    const panel = page.getByTestId('circuits-probe-tab');
    const bounds = await panel.evaluate(root => ({ width: root.getBoundingClientRect().width, scroll: root.scrollWidth, client: root.clientWidth }));
    expect(bounds.width).toBeLessThanOrEqual(240);
    expect(bounds.scroll).toBeLessThanOrEqual(bounds.client);
    await complete.scrollIntoViewIfNeeded();
    await panel.screenshot({ path: `${out}/handoff-checkpoint.png` });
    await complete.click();
    await expect.poll(() => db.prepare('SELECT state FROM autopilot_circuit_runs WHERE id=?').get(run).state,
      { timeout: 20000 }).toBe('completed');
    expect(db.prepare("SELECT COUNT(*) AS count FROM circuit_run_history WHERE run_id=? AND kind='operator_attestation' AND source='operator'").get(run).count).toBe(1);
    expect(db.prepare("SELECT status FROM autopilot_circuit_run_steps WHERE run_id=? AND node_id='done'").get(run).status).toBe('completed');
    expect(db.prepare("SELECT error_message FROM autopilot_circuit_run_steps WHERE run_id=? AND node_id='await_source'").get(run).error_message).toBeNull();
    expect(db.prepare('SELECT COUNT(*) AS count FROM circuit_effects WHERE run_id=?').get(run).count).toBe(0);
    await page.getByTestId('circuits-view-history').click();
    await expect(page.getByTestId(`run-state-${run}`)).toHaveText('Completed');
    for (const dismiss of await page.getByRole('button', { name: 'Dismiss notification', exact: true }).all()) {
      await dismiss.click();
    }
    await panel.screenshot({ path: `${out}/handoff-completed.png` });
    console.log('PASS: real IPC manual attestation + worker tick completed the run; no external effects, 240px panel');
  } finally {
    db.close();
    await invoke('delete_mesh', { meshId: mesh.id });
  }
}
