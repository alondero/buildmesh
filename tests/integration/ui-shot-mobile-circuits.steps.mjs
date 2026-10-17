import { expect } from '@playwright/test';

export default async function ({ page }) {
  const mesh = { id: 3, name: 'Phone reviews', path: '/repo', scratchpad: '' };
  const node = { id: 17, mesh_id: 3, name: 'Fix retry loop', provider: 'claude', branch: 'feature/retry', status: 'running' };
  const gate = { node_id: 'confirm_source', role: 'Approval gate', attempt: 1, status: 'blocked' };
  let run = { node_id: 17, run_id: 7, blueprint_name: 'Team Review Circuit', state: 'running', step: gate,
    review: { round: 2, max_rounds: 3, verdict: 'changes_requested', summary: 'The retry loop must stop when the desktop disconnects.' },
    needs_you: [{ kind: 'approval', step: gate, detail: null, actions: [], agent_node_id: null }], revision: 12 };
  const writes = [];
  let refuse = true;
  let finishApproval;
  const approvalReply = new Promise(resolve => { finishApproval = resolve; });
  await page.routeWebSocket('**/ws/events**', () => {});
  await page.route('**/api/**', async route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === '/api/session') return route.fulfill({ status: 204 });
    if (path === '/api/ws-ticket') return route.fulfill({ json: { ticket: 'fixture' } });
    if (request.method() === 'POST') {
      const body = request.postDataJSON();
      writes.push({ path, body });
      if (path === '/api/circuit-runs/7/approve') {
        await approvalReply;
        run = { ...run, needs_you: [], step: { ...gate, status: 'completed' }, revision: 13 };
      } else if (path === '/api/nodes/17/reviews' && refuse) {
        refuse = false;
        return route.fulfill({ status: 409, json: { error: 'Resume the agent before starting a review.' } });
      }
      return route.fulfill({ json: { ok: true, run_id: 7 } });
    }
    const body = path === '/api/meshes' ? [mesh] : path === '/api/nodes' ? [node]
      : path === '/api/providers' ? [] : path === '/api/circuit-runs' ? [run]
      : path === '/api/nodes/17/reviews' ? [{ circuit_id: null, name: 'Built-in Review Blueprint' }, { circuit_id: 9, name: 'Team review' }]
      : undefined;
    if (body === undefined) return route.fulfill({ status: 503, json: { error: 'Fixture endpoint unavailable' } });
    return route.fulfill({ json: body });
  });
  await page.goto(`${new URL(page.url()).origin}/mobile/index.html`, { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: 'Work', exact: true }).click();
  await page.getByRole('button', { name: 'Needs attention', exact: true }).click();
  await expect(page.getByTestId('node-circuit-17')).toContainText('Round 2 of 3');
  await page.getByTestId('node-17').click();
  await expect(page.getByTestId('circuit-run-panel')).toBeVisible();
  await page.getByText('Reviewer’s latest report').click();
  await page.screenshot({ path: '.tmp/mobile-circuit-run.png' });
  await page.getByRole('button', { name: 'Approve…', exact: true }).click();
  expect(writes).toHaveLength(0);
  await page.getByRole('button', { name: 'Confirm', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Sending…' })).toBeDisabled();
  await page.evaluate(() => new Promise(resolve => {
    window.addEventListener('popstate', () => resolve(), { once: true });
    window.history.back();
  }));
  await expect(page.getByRole('dialog').getByRole('button', { name: 'Sending…' })).toBeVisible();
  finishApproval();
  await expect(page.getByTestId('circuit-run-line')).toContainText('Approval gate: Done');
  expect(writes).toEqual([{ path: '/api/circuit-runs/7/approve', body: { node_id: 'confirm_source' } }]);
  await page.getByRole('button', { name: 'Start review…' }).click();
  await page.getByLabel('Review Circuit').selectOption('9');
  await page.getByRole('button', { name: 'Start review', exact: true }).click();
  await expect(page.getByRole('dialog').getByRole('alert')).toContainText('Resume the agent');
  await page.setViewportSize({ width: 320, height: 740 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  for (const control of await page.getByRole('dialog').locator('button, select, input').all()) {
    const box = await control.boundingBox();
    expect(box).not.toBeNull();
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(320);
  }
  await page.screenshot({ path: '.tmp/mobile-start-review-refusal.png' });
  await page.getByLabel('Review Circuit').selectOption('');
  await page.getByLabel('Review rounds').fill('4');
  await page.getByRole('button', { name: 'Start review', exact: true }).click();
  await expect(page.getByText('Review started. An existing live run is kept.')).toBeVisible();
  expect(writes.at(-1)).toEqual({ path: '/api/nodes/17/reviews', body: { circuit_id: null, max_rounds: 4 } });
}
