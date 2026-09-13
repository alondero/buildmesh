import { expect } from '@playwright/test';
import { DatabaseSync } from 'node:sqlite';
import path from 'node:path';

// Real dev-profile IPC, with an archived source lacking a saved session.
// This exercises the recovery error without launching a paid agent.
export default async function ({ page, invoke }) {
  const mesh = await invoke('create_test_mesh', { name: 'Review continuation verification' });
  const db = new DatabaseSync(path.join(process.env.APPDATA, 'com.alond.buildmesh.dev', 'buildmesh.db'));
  try {
    db.prepare('UPDATE meshes SET path=?, pre_spawn_pool_size=0 WHERE id=?').run(process.cwd(), mesh.id);
    const source = Number(db.prepare("INSERT INTO agent_nodes (mesh_id,name,path,branch,env,provider,status) VALUES (?,'Retained implementation',?,'main','windows','codex','archived')")
      .run(mesh.id, process.cwd()).lastInsertRowid);
    const graph = JSON.stringify({ version: 3, nodes: [
      { id: 'trigger', type: { type: 'manual' } },
      { id: 'reviewer', type: { type: 'spawn_agent_node', prompt: 'Review {{source.path}}', name: 'Reviewer', provider: 'codex', model: null, effort: null, extra_args: null, timeout_seconds: null } },
      { id: 'verdict', type: { type: 'review_verdict', target_node_id: 'reviewer' } },
      { id: 'feedback', type: { type: 'inject_pty', prompt: 'Address {{node.reviewer.output}}', target_node_id: '$source' } },
      { id: 'retry', type: { type: 'retry_limit', max_retries: 3 } },
    ], edges: [] });
    const circuit = Number(db.prepare("INSERT INTO autopilot_circuits (mesh_id,name,enabled,concurrency_limit,graph_json,is_preset) VALUES (?,'Parser review',0,2,?,1)").run(mesh.id, graph).lastInsertRowid);
    const context = JSON.stringify({ 'source.review_preset': '1', 'source.agent_id': String(source), 'source.path': process.cwd(), 'node.verdict.review_verdict_attempt': '3', 'node.reviewer.output': 'The latest fixes address the parsing issue. Another pass is needed to verify the error handling.' });
    const run = Number(db.prepare("INSERT INTO autopilot_circuit_runs (circuit_id,mesh_id,source_agent_node_id,trigger_identity,state,context_json) VALUES (?,?,?,'manual:continuation-shot','failed',?)").run(circuit, mesh.id, source, context).lastInsertRowid);
    const step = db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt,outcome) VALUES (?,?,'completed',3,?)");
    step.run(run, 'verdict', 'working');
    step.run(run, 'retry', 'failed');
    const approvalGraph = JSON.stringify({ version: 3, nodes: [
      { id: 'trigger', type: { type: 'manual' } },
      { id: 'trust', type: { type: 'collaborator_check', require_approval: true } },
    ], edges: [{ from: 'trigger', to: 'trust', condition: 'always' }] });
    const approvalCircuit = Number(db.prepare("INSERT INTO autopilot_circuits (mesh_id,name,enabled,concurrency_limit,graph_json) VALUES (?,'Authorization gate',0,1,?)").run(mesh.id, approvalGraph).lastInsertRowid);
    const approvalRun = Number(db.prepare("INSERT INTO autopilot_circuit_runs (circuit_id,mesh_id,trigger_identity,state,context_json) VALUES (?,?,'manual:approval-shot','running','{}')").run(approvalCircuit, mesh.id).lastInsertRowid);
    db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt,outcome) VALUES (?,'trigger','completed',1,'completed')").run(approvalRun);
    db.prepare("INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt) VALUES (?,'trust','blocked',1)").run(approvalRun);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator(`#mesh-item-name-${mesh.id}`).click();
    await page.getByRole('button', { name: 'Search or open' }).click();
    await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
    // The bare "Open Circuits" label now also matches the new mesh-scoped
    // entries ("Open Circuits in <Mesh>"). Anchor on the canonical command's
    // distinctive label+subtitle prefix "Open Circuits Inspect…" so only the
    // app-wide command is selected (the per-mesh entries start with
    // "Open Circuits in ").
    await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await page.keyboard.press('End');
    await page.getByTestId('circuits-view-history').click();
    await expect(page.getByTestId(`run-state-${run}`)).toHaveText('Review limit reached');
    const button = page.getByTestId(`run-continue-review-${run}`);
    await expect(button).toBeVisible();
    await page.getByTestId('circuits-probe-tab').screenshot({ path: 'docs/pr-screenshots/maddened-dowdy-strand/review-continuation.png' });
    await button.click();
    await expect(page.getByText(/Could not resume the implementation agent:/)).toBeVisible();
    await expect(button).toBeEnabled();
    expect(db.prepare('SELECT state FROM autopilot_circuit_runs WHERE id=?').get(run).state).toBe('failed');
    expect(db.prepare('SELECT COUNT(*) AS n FROM autopilot_circuit_runs WHERE mesh_id=?').get(mesh.id).n).toBe(2);
    const bounds = await page.getByTestId('circuits-probe-tab').evaluate((root) => {
      const rect = root.getBoundingClientRect();
      return { width: rect.width, overflow: root.scrollWidth > root.clientWidth,
        clipped: [...root.querySelectorAll('button')].some((b) => { const r = b.getBoundingClientRect(); return r.width > 0 && (r.left < rect.left || r.right > rect.right); }) };
    });
    expect(bounds.width).toBeLessThanOrEqual(240);
    expect(bounds.overflow).toBe(false);
    expect(bounds.clipped).toBe(false);
    await page.getByTestId('circuits-probe-tab').screenshot({ path: 'docs/pr-screenshots/maddened-dowdy-strand/review-continuation-error.png' });
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator(`#mesh-item-name-${mesh.id}`).click();
    await page.getByRole('button', { name: 'Search or open' }).click();
    await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
    // The bare "Open Circuits" label now also matches the new mesh-scoped
    // entries ("Open Circuits in <Mesh>"). Anchor on the canonical command's
    // distinctive label+subtitle prefix "Open Circuits Inspect…" so only the
    // app-wide command is selected (the per-mesh entries start with
    // "Open Circuits in ").
    await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
    await page.getByTestId('circuits-view-activity').click();
    await expect(page.getByText(/Waiting does not expire/)).toBeVisible();
    const approve = page.getByTestId(`approve-${approvalRun}-trust`);
    await expect(approve).toBeEnabled();
    await page.getByTestId('circuits-probe-tab').screenshot({ path: 'docs/pr-screenshots/maddened-dowdy-strand/approval-wait.png' });
    await approve.click();
    await expect.poll(() => db.prepare("SELECT status FROM autopilot_circuit_run_steps WHERE run_id=? AND node_id='trust'").get(approvalRun).status).toBe('completed');
  } finally {
    db.close();
    await invoke('delete_mesh', { meshId: mesh.id });
  }
}
