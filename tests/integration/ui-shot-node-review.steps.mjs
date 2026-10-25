import { expect } from '@playwright/test';

export default async function ({ page, mock }) {
  await page.locator('#mesh-item-name-1').click();
  const card = page.locator('[data-node-card-id="1"]');
  const summary = card.getByRole('region', { name: 'Node review circuit' });
  await expect(summary.getByText('No circuit attached')).toBeVisible();
  await summary.getByRole('button', { name: 'Add review' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('Reviewer launch configuration')).toBeVisible();
  await dialog.getByRole('button', { name: 'Claude configurations' }).click();
  await page.getByRole('menuitem', { name: 'Parser review configuration', exact: true }).click();
  await page.getByRole('menu', { name: 'Claude configurations', exact: true }).press('Escape');
  await expect(dialog.getByTestId('reviewer-provider-selection')).toHaveText('Parser review configuration');
  await dialog.getByLabel('Maximum review rounds').fill('0');
  await expect(dialog.getByRole('button', { name: 'Start review', exact: true })).toBeDisabled();
  await dialog.getByLabel('Maximum review rounds').fill('5');
  await expect(dialog.getByRole('button', { name: 'Start review', exact: true })).toBeEnabled();
  await dialog.screenshot({ path: '.tmp/node-review-setup.png' });
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();

  const ownership = { node_id: 1, run_id: 91, circuit_id: 3, circuit_name: 'Parser review', state: 'running', parent_node_id: null };
  const overview = {
    circuit_name: 'Parser review', review_round_limit: 5,
    run: { id: 91, circuit_id: 3, mesh_id: 1, source_agent_node_id: 1, trigger_identity: 'manual:1', state: 'running',
      context_json: '{"retry.attempt":"2","retry.max_retries":"5"}', created_at: '2026-10-25 10:00:00', updated_at: '2026-10-25 10:05:00', attention_dismissed_at: null },
    graph: { version: 3, nodes: [{ id: 'await_source', type: { type: 'await_agent_turn', target_node_id: 'source' } }], edges: [] },
    steps: [{ id: 1, run_id: 91, node_id: 'await_source', status: 'running', attempt: 1, agent_node_id: 1,
      outcome: null, error_message: null, started_at: '2026-10-25 10:00:00', completed_at: null }],
    attention: { revision: 1, checkpoints: [], pushes: [] },
  };
  await mock.on('list_circuit_agent_ownerships', [ownership]);
  await mock.on('circuit_run_overview', overview);
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'running' });
  await expect(summary.getByText('Running', { exact: true })).toBeVisible();
  await expect(summary.getByText('Round 2 of 5')).toBeVisible();
  await expect(summary.getByRole('button', { name: 'Add review' })).toHaveCount(0);
  await expect(card.locator('.xterm')).toBeVisible();
  await summary.getByRole('button', { name: 'View circuit' }).click();
  await expect(card.getByTestId('run-overview-91')).toBeVisible();
  await card.getByRole('tab', { name: /^Agent|^Implementation/ }).click();
  await expect(summary).toBeVisible();

  await mock.on('circuit_run_overview', { ...overview, run: { ...overview.run, state: 'paused' } });
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'paused' });
  await expect(summary.getByText('Paused', { exact: true })).toBeVisible();
  // At narrow widths, the summary wraps and its actions remain inside the card.
  await card.evaluate(el => { el.style.width = '260px'; el.style.flex = 'none'; });
  expect(await summary.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await summary.screenshot({ path: '.tmp/node-review-narrow.png' });
  const longName = 'ParserReview'.repeat(10);
  await mock.on('list_circuit_agent_ownerships', [{ ...ownership, circuit_name: longName, state: 'paused' }]);
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'paused' });
  await expect(summary.getByText(longName, { exact: true })).toBeVisible();
  expect(await summary.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  const link = await summary.getByRole('button', { name: 'View circuit' }).boundingBox();
  const bounds = await summary.boundingBox();
  expect(link.x + link.width).toBeLessThanOrEqual(bounds.x + bounds.width);
  await summary.screenshot({ path: '.tmp/node-review-long-name.png' });
  await mock.on('list_circuit_agent_ownerships', [{ ...ownership, state: 'paused' }]);
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'paused' });
  await expect(summary.getByText('Parser review', { exact: true })).toBeVisible();
  await card.evaluate(el => { el.style.width = ''; el.style.flex = ''; });
}
