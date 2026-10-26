import { expect } from '@playwright/test';

/** Pixels between the card's top edge and its activity panel (terminal or Overview). */
const chromeAboveTerminal = card => card.evaluate(el =>
  el.querySelector('[id^="activity-panel-"]').getBoundingClientRect().top - el.getBoundingClientRect().top);

export default async function ({ page, mock }) {
  await page.locator('#mesh-item-name-1').click();
  const card = page.locator('[data-node-card-id="1"]');
  await expect(card.locator('.xterm')).toBeVisible();
  // No circuit: the terminal sits directly under the title bar.
  await expect(card.getByText('No circuit attached')).toHaveCount(0);
  await expect(card.getByRole('tablist')).toHaveCount(0);
  const idleChrome = await chromeAboveTerminal(card);
  expect(idleChrome).toBeLessThanOrEqual(32);
  await card.screenshot({ path: '.tmp/node-review-idle.png' });

  // The draggable title bar has role=button, so ARIA hides its nested buttons from role queries.
  const review = card.locator('[aria-label="Start review or circuit"]');
  await expect(review).toHaveText('Review');
  await review.click();
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
  // Circuit status rides on the Overview tab; the terminal stays on screen.
  const overviewTab = card.getByRole('tab', { name: /^Circuit run overview/ });
  await expect(overviewTab).toHaveText('Running · 2/5');
  await expect(overviewTab).toHaveAttribute('title', /Parser review · Running · Round 2 of 5 · Await task/);
  await expect(card.getByRole('region', { name: 'Node review circuit' })).toHaveCount(0);
  await expect(card.locator('.xterm')).toBeVisible();
  const circuitChrome = await chromeAboveTerminal(card);
  expect(circuitChrome).toBeLessThanOrEqual(56);
  await card.screenshot({ path: '.tmp/node-review-circuit.png' });
  await overviewTab.click();
  await expect(card.getByTestId('run-overview-91')).toBeVisible();
  await card.getByRole('tab', { name: /^Agent|^Implementation/ }).click();
  await expect(card.locator('.xterm')).toBeVisible();

  await mock.on('circuit_run_overview', { ...overview, run: { ...overview.run, state: 'paused' } });
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'paused' });
  await expect(overviewTab).toHaveText('Paused · 2/5');
  // At a narrow card, the review button drops its text and the tab strip stays inside the card.
  await card.evaluate(el => { el.style.width = '260px'; el.style.flex = 'none'; });
  await expect(review).toHaveText('');
  const longName = 'ParserReview'.repeat(10);
  await mock.on('list_circuit_agent_ownerships', [{ ...ownership, circuit_name: longName, state: 'paused' }]);
  await mock.on('circuit_run_overview', { ...overview, circuit_name: longName, run: { ...overview.run, state: 'paused' } });
  await mock.emit('circuit-run-updated', { run_id: 91, state: 'paused' });
  await expect(overviewTab).toHaveAttribute('title', new RegExp(longName));
  expect(await card.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  const tabBox = await overviewTab.boundingBox();
  const cardBox = await card.boundingBox();
  expect(tabBox.x + tabBox.width).toBeLessThanOrEqual(cardBox.x + cardBox.width);
  await card.screenshot({ path: '.tmp/node-review-narrow.png' });
  await card.evaluate(el => { el.style.width = ''; el.style.flex = ''; });
}
