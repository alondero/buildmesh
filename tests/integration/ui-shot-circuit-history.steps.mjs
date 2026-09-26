import { expect } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import path from 'node:path';

// Dev-profile fixture only (#1909). Seeds one synthetic run per lifecycle state
// — working, waiting (step capacity + queue admission), Unverified, failed and
// recovery — through the real dev DB, then drives the real WebView2 Probe at
// 240px and checks each state's next safe action, reason and history through
// the registered backend IPC. No agents or enabled triggers are created.
//
// The graph is chosen so the running worker leaves the fixtures alone:
//   * `inject_pty` on a running step with no bound agent dispatches nothing;
//   * `llm_turn_classifier` on an Unverified step yields a read-only Recheck;
//   * `concurrency_limit = 1` with one running step keeps the queued step parked
//     and the mesh `circuit_run_capacity = 1` keeps the pending run queued.
const GRAPH = JSON.stringify({
  version: 2,
  nodes: [
    { id: 'work', type: { type: 'inject_pty', prompt: '' } },
    { id: 'reviewer', type: { type: 'llm_turn_classifier', target_node_id: null } },
  ],
  edges: [],
});

export default async function ({ page, invoke }) {
  const mesh = await invoke('create_test_mesh', { name: 'Circuit history states' });
  const db = new DatabaseSync(path.join(process.env.APPDATA, 'com.alond.buildmesh.dev', 'buildmesh.db'));
  try {
    db.prepare('UPDATE meshes SET path=?, pre_spawn_pool_size=0, circuit_run_capacity=1 WHERE id=?')
      .run(process.cwd(), mesh.id);
    const circuit = db.prepare("INSERT INTO autopilot_circuits (mesh_id,name,graph_json,enabled,concurrency_limit) VALUES (?,?,?,0,1)")
      .run(mesh.id, 'Lifecycle states', GRAPH).lastInsertRowid;
    const insertRun = db.prepare('INSERT INTO autopilot_circuit_runs (circuit_id,mesh_id,trigger_identity,state,context_json) VALUES (?,?,?,?,?)');
    const insertStep = db.prepare('INSERT INTO autopilot_circuit_run_steps (run_id,node_id,status,attempt,outcome,error_message) VALUES (?,?,?,?,?,?)');
    const insertHistory = db.prepare('INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail,source,disposition) VALUES (?,?,?,?,?,?,?)');

    // Working: a running step, no wait.
    const working = insertRun.run(circuit, mesh.id, 'manual:state-working', 'running', '{}').lastInsertRowid;
    insertStep.run(working, 'work', 'running', 1, null, null);
    insertHistory.run(working, null, null, 'configuration_pinned',
      JSON.stringify({ behavior_revision: 1, graph_sha256: 'abcdef0123456789', reviewers: [] }),
      'run.configuration', 'applied');

    // Waiting: a step parked on the circuit's single step slot.
    const waiting = insertRun.run(circuit, mesh.id, 'manual:state-waiting', 'running', '{}').lastInsertRowid;
    insertStep.run(waiting, 'work', 'pending_slot', 1, null, null);
    insertHistory.run(waiting, 'work', 1, 'step_capacity_wait',
      JSON.stringify({ before: null, after: JSON.stringify({ circuit_limit: true, agent_limit: false }) }),
      'circuit_worker.capacity', 'waiting');

    // Unverified: a checkpoint with a read-only Recheck action.
    const unverified = insertRun.run(circuit, mesh.id, 'manual:state-unverified', 'running', '{}').lastInsertRowid;
    insertStep.run(unverified, 'reviewer', 'unverified', 1, null,
      'Evidence is incomplete. Inspect the latest observations before continuing.');

    // Failed: a terminal failure with a readable reason.
    const failed = insertRun.run(circuit, mesh.id, 'manual:state-failed', 'failed', '{}').lastInsertRowid;
    insertStep.run(failed, 'reviewer', 'failed', 1, 'failed', 'The review command exited before producing a result.');

    // Recovery: a failed predecessor recording its successor, and the successor
    // pointing back at it.
    const predecessor = insertRun.run(circuit, mesh.id, 'manual:state-recovery-a', 'failed', '{}').lastInsertRowid;
    insertStep.run(predecessor, 'reviewer', 'failed', 1, 'failed', 'No final approval is recorded.');
    const successor = insertRun.run(circuit, mesh.id, 'manual:state-recovery-b', 'paused',
      JSON.stringify({ 'recovery.from_run_id': String(predecessor) })).lastInsertRowid;
    insertHistory.run(predecessor, null, null, 'recovery',
      JSON.stringify({ successor_run_id: successor, rounds: 2 }), 'operator', 'applied');

    // Waiting (admission): a pending run held by the mesh's single run slot.
    const queued = insertRun.run(circuit, mesh.id, 'manual:state-queued', 'pending', '{}').lastInsertRowid;

    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator(`#mesh-item-name-${mesh.id}`).click();
    await page.getByRole('button', { name: 'Search or open' }).click();
    await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
    await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
    await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
    await page.keyboard.press('End');
    await expect(page.getByTestId('circuits-probe-tab')).toBeVisible();

    const shotDir = path.join(process.cwd(), '.tmp');
    await mkdir(shotDir, { recursive: true });

    // --- Activity: working, waiting, Unverified, recovery link ---------------
    await expect(page.getByTestId(`run-card-${working}`)).toBeVisible();
    await expect(page.getByTestId(`run-activity-${working}`)).toContainText('Running');
    await expect(page.getByTestId(`run-activity-${waiting}`)).toContainText('Queued');
    await expect(page.getByTestId(`run-reason-${waiting}`)).toContainText(/Waiting for a slot/);
    await expect(page.getByTestId(`run-activity-${unverified}`)).toContainText('Unverified Checkpoint');
    await expect(page.getByTestId(`run-reason-${unverified}`)).toContainText(/Evidence is incomplete/);
    await expect(page.getByText(new RegExp(`Continues run #${predecessor}`))).toBeVisible();

    // The Unverified next safe action: open the card's history and its Recheck.
    const unverifiedCard = page.getByTestId(`run-card-${unverified}`);
    const unverifiedToggle = page.getByTestId(`run-toggle-${unverified}`);
    if ((await unverifiedToggle.getAttribute('aria-expanded')) !== 'true') await unverifiedToggle.click();
    await unverifiedCard.getByText('Circuit Run History').click();
    const recheck = unverifiedCard.getByRole('button', { name: 'Recheck evidence' });
    await expect(recheck).toBeVisible();
    await recheck.scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(shotDir, '1909-states-activity.png') });

    // --- History: failed reason + recovery successor reference ---------------
    await page.getByTestId('circuits-view-history').click();
    await expect(page.getByTestId(`run-card-${failed}`)).toBeVisible();
    await expect(page.getByTestId(`run-error-${failed}`)).toContainText('The review command exited');
    await expect(page.getByTestId(`run-activity-${failed}`)).toContainText('Failed');
    const predecessorCard = page.getByTestId(`run-card-${predecessor}`);
    const predecessorToggle = page.getByTestId(`run-toggle-${predecessor}`);
    if ((await predecessorToggle.getAttribute('aria-expanded')) !== 'true') await predecessorToggle.click();
    await predecessorCard.getByText('Circuit Run History').click();
    await expect(predecessorCard.getByText(new RegExp(`Recovered into run #${successor}`))).toBeVisible();
    await expect(predecessorCard.getByText('Review recovery')).toBeVisible();
    await page.screenshot({ path: path.join(shotDir, '1909-states-history.png') });

    // --- Queue: the pending run's admission reason ---------------------------
    await page.getByTestId('circuits-view-queue').click();
    await expect(page.getByTestId(`queue-run-${queued}`)).toBeVisible();
    await expect(page.getByTestId(`queue-pending-reason-${queued}`)).toBeVisible();
    await page.screenshot({ path: path.join(shotDir, '1909-states-queue.png') });

    // --- 240px acceptance ----------------------------------------------------
    await page.getByTestId('circuits-view-activity').click();
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
    console.log(`CIRCUIT_HISTORY_STATES_MESH=${mesh.id} WORKING=${working} WAITING=${waiting} UNVERIFIED=${unverified} FAILED=${failed} PREDECESSOR=${predecessor} SUCCESSOR=${successor} QUEUED=${queued}`);
  } finally {
    db.close();
    // The fixture creates durable circuit rows directly; remove the mesh after
    // the DB handle is closed (mesh deletion cascades circuit/run/history).
    try {
      await invoke('delete_mesh', { meshId: mesh.id });
    } catch (error) {
      console.warn(`Could not clean up circuit history states mesh ${mesh.id}:`, error);
    }
  }
}
