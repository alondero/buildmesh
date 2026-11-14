// Frontend-only evidence for continuing the same review from either surface.
import { expect } from '@playwright/test';
import { graph, scenarios, step } from './ui-shot-circuit-overview.fixtures.mjs';

const continuation = {
  lineage: { review_origin: 'local_preset', extended: false, recovery: { kind: 'none' } },
  supported: true, available: true, refusal_reason: null, refusal_message: null,
  source_agent_node_id: 1, reviewer_agent_node_id: 2, target_run_id: 91,
  review_nodes: { verdict_node_id: 'verdict', retry_node_ids: ['retry'], merge_node_id: 'merge',
    merge_confirmed_node_id: null, merge_unconfirmed_node_ids: [] },
};
const failed = {
  ...scenarios.reviewing,
  run: { ...scenarios.reviewing.run, state: 'failed' },
  steps: [...scenarios.reviewing.steps.filter((s) => !['verdict', 'retry'].includes(s.node_id)),
    step('verdict', 'completed', { outcome: 'working', attempt: 5 }),
    step('retry', 'completed', { outcome: 'failed', attempt: 5 })],
  attention: { revision: 42, checkpoints: [], pushes: [] },
  review_continuation: continuation,
};
failed.run.context_json = JSON.stringify({ ...JSON.parse(failed.run.context_json),
  'retry.attempt': '5', 'node.verdict.review_verdict_attempt': '5' });

async function fits(panel) {
  expect(await panel.evaluate((el) => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(0);
  for (const control of [panel.getByLabel('Additional review rounds'), panel.getByRole('button', { name: /Review again/ })]) {
    const box = await control.boundingBox();
    const bounds = await panel.boundingBox();
    expect(box.width).toBeGreaterThanOrEqual(24);
    expect(box.height).toBeGreaterThanOrEqual(24);
    expect(box.x).toBeGreaterThanOrEqual(bounds.x);
    expect(box.x + box.width).toBeLessThanOrEqual(bounds.x + bounds.width);
  }
}

export default async function ({ page, mock }) {
  await mock.on('list_circuit_agent_ownerships', [{ node_id: 1, run_id: 91, circuit_id: 3,
    circuit_name: 'Review', state: 'failed', parent_node_id: null }]);
  await mock.on('circuit_run_overview', failed);
  await mock.on('circuit_run_history', { entries: [], checkpoints: [], coverage: [] });
  await page.locator('#mesh-item-name-1').click();
  const card = page.locator('[data-node-card-id="1"]');
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'failed' });
  await card.getByRole('tab', { name: /^Circuit run overview/ }).click();
  const panel = card.getByTestId('run-overview-91');
  await expect(panel).toHaveAttribute('data-run-state', 'failed');
  if (process.env.REVIEW_EXTENSION_BASELINE === '1') {
    await expect(panel.getByRole('button', { name: /Review again/ })).toHaveCount(0);
    console.log('Baseline: exhausted review has no continuation action on Overview.');
    return;
  }
  await expect(panel.getByRole('button', { name: 'Review again · +1 round' })).toBeVisible();
  await expect(panel.getByRole('list', { name: 'Circuit steps' })).toHaveCount(0);
  await card.evaluate((el) => { el.style.width = '260px'; el.style.flex = 'none'; });
  for (const theme of ['dark', 'light']) {
    await page.evaluate((value) => document.documentElement.setAttribute('data-theme', value), theme);
    await fits(panel);
    await panel.screenshot({ path: `.tmp/review-extension-overview-${theme}.png` });
  }
  await page.evaluate(() => window.__BUILDMESH_MOCK__.on('continue_circuit_review', () => {
    throw new Error('The retained reviewer is unavailable. Open the implementation agent and retry this review.');
  }));
  await panel.getByRole('button', { name: 'Review again · +1 round' }).click();
  await expect(panel.getByRole('alert')).toContainText('The retained reviewer is unavailable.');
  await expect(panel).toHaveAttribute('data-run-state', 'failed');
  await expect(panel.getByRole('button', { name: 'Review again · +1 round' })).toBeEnabled();
  await fits(panel);
  await panel.getByLabel('Additional review rounds').selectOption('3');
  // Keep command completion under test control and record the actual IPC args.
  await page.evaluate(() => {
    window.__BUILDMESH_MOCK__.on('continue_circuit_review', (args) => new Promise((resolve) => {
      window.extensionArgs = args;
      window.finishExtension = resolve;
    }));
  });
  await panel.getByRole('button', { name: 'Review again · +3 rounds' }).click();
  await expect(panel.getByLabel('Additional review rounds')).toBeDisabled();
  await expect(panel.getByRole('button', { name: 'Review again · +3 rounds' })).toBeDisabled();
  expect(await page.evaluate(() => window.extensionArgs)).toEqual({ runId: 91, additionalRounds: 3 });
  await mock.on('circuit_run_overview', { ...failed, run: { ...failed.run, state: 'running' },
    review_continuation: { ...continuation, available: false, refusal_reason: 'run_not_failed' } });
  await page.evaluate(() => window.finishExtension(91));
  await expect(panel).toHaveAttribute('data-run-state', 'running');
  await expect(panel.getByLabel('Additional review rounds')).toHaveCount(0);

  const circuit = { id: 3, mesh_id: 1, name: 'Review', description: '', enabled: true,
    is_preset: true, graph_json: JSON.stringify(graph), created_at: '', updated_at: '' };
  await mock.on('list_circuit_probe', { circuits: [{ circuit, runs: [failed] }], queue: [], queue_waits: {} });
  await page.getByRole('button', { name: 'Search or open' }).click();
  await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
  await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
  await page.getByTestId('circuits-view-history').click();
  await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
  await page.keyboard.press('End');
  const probe = page.getByTestId('run-card-91');
  await expect(page.getByTestId('run-toggle-91')).toHaveAttribute('aria-expanded', 'false');
  for (const theme of ['dark', 'light']) {
    for (const scale of ['1', '1.5']) {
      await page.evaluate(([name, size]) => {
        document.documentElement.setAttribute('data-theme', name);
        document.documentElement.style.setProperty('--ui-text-scale', size);
      }, [theme, scale]);
      await fits(probe);
    }
  }
  await probe.getByLabel('Additional review rounds').selectOption('10');
  await probe.getByRole('button', { name: 'Review again · +10 rounds' }).click();
  expect(await page.evaluate(() => window.extensionArgs)).toEqual({ runId: 91, additionalRounds: 10 });
  await expect(probe.getByRole('button', { name: 'Review again · +10 rounds' })).toBeDisabled();
  await page.evaluate(() => window.finishExtension(91));
  await mock.on('circuit_run_overview', failed);
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'failed' });
  await expect(panel).toHaveAttribute('data-run-state', 'failed');
  await page.evaluate(() => {
    document.documentElement.removeAttribute('data-theme');
    document.documentElement.style.removeProperty('--ui-text-scale');
  });
  await card.evaluate((el) => { el.style.width = ''; el.style.flex = ''; });
  await panel.getByRole('button', { name: /Review again/ }).scrollIntoViewIfNeeded();
  console.log('PASS: Overview and collapsed Probe offer review extension; IPC, pending controls, refresh and narrow layout checked with mock IPC.');
}
