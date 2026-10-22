import { expect } from '@playwright/test';

export default async function ({ page }) {
  const mesh = { id: 3, name: 'Phone triage', path: '/repo', created_at: '', scratchpad: '', sandbox: false };
  const issue = { number: 42, title: 'Fix the mobile deadline and preserve a long issue title on a narrow phone screen', body: 'Expanded issue details', url: 'https://github.com/o/r/issues/42', state: 'open', labels: ['bug'], blocked_by: [43, 99], author: 'octo' };
  const providers = [
    { id: 'claude', label: 'Claude Code', capabilities: { supports_prefill: true }, configuration: null, harness_id: 'claude' },
    { id: 'terminal', label: 'Terminal', capabilities: { supports_prefill: false } },
    { id: 'saved', label: 'Saved recipe', capabilities: { supports_prefill: true }, configuration: { id: 'saved', name: 'Saved recipe', spawn_option_id: 'claude' } },
  ];
  const writes = [];
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    let body;
    if (path === '/api/session') return route.fulfill({ status: 204 });
    if (path === '/api/meshes') body = [mesh];
    else if (path === '/api/nodes') body = [];
    else if (path === '/api/providers') body = providers;
    else if (path === '/api/meshes/3/issues') body = {
      items: [issue, { ...issue, number: 43, title: 'Prerequisite', body: '', labels: [], blocked_by: [], author: '' }],
      completeness: { returned: 2, pages_fetched: 1, complete: false, incomplete_reason: 'safety_cap', reported_total: 120 },
    };
    else if (path === '/api/meshes/3/issue-labels') body = {
      labels: ['bug', 'buildmesh:run', 'a-very-long-repository-label-that-must-wrap-at-the-narrow-phone-width'],
      autopilot_triggers: { 'buildmesh:run': ['Ship fixes'] },
    };
    else if (path === '/api/meshes/3/issues/42/label') {
      const write = route.request().postDataJSON();
      writes.push(write);
      issue.labels = [...issue.labels.filter(label => label !== write.label), ...(write.present ? [write.label] : [])];
      return route.fulfill({ status: 204 });
    } else return route.fulfill({ status: 503, json: { error: 'Fixture endpoint unavailable' } });
    await route.fulfill({ json: body });
  });
  const base = new URL(page.url()).origin;
  await page.goto(`${base}/mobile/index.html`, { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: 'Work', exact: true }).click();
  await page.getByTestId('mesh-actions-3').click();
  await page.getByTestId('mesh-sheet-issues').click();
  await expect(page.getByTestId('issue-42')).toBeVisible();
  await expect(page.getByTestId('issue-body-42')).toHaveCount(0);
  await expect(page.getByText('Blocked by #43', { exact: true })).toBeVisible();
  await expect(page.getByTestId('feed-completeness-note')).toContainText('Showing the first 2 of 120 issues.');
  const fits = async () => {
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    for (const box of await page.locator('input.field, select.field, [role="checkbox"]').all()) {
      const bounds = await box.boundingBox();
      if (bounds) expect(bounds.x + bounds.width).toBeLessThanOrEqual(page.viewportSize().width);
    }
  };
  await fits();
  await page.screenshot({ path: '.tmp/issues-web-collapsed.png' });
  await page.getByTestId('issues-filter').fill(' BUG ');
  await expect(page.getByTestId('issue-43')).toHaveCount(0);
  await page.getByTestId('issues-filter').fill('no match');
  await expect(page.getByText('No matches.')).toBeVisible();
  await expect(page.getByTestId('feed-completeness-note')).toBeVisible();
  await page.getByTestId('issues-filter').fill('');
  await page.getByTestId('issue-42').click();
  await expect(page.getByTestId('issue-body-42')).toBeVisible();
  await page.getByTestId('issue-spawn-42-menu').click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText('Claude Code', { exact: true })).toBeVisible();
  // A saved Launch Configuration is a menu row of its own (issue #2385).
  await expect(dialog.getByText('Saved recipe', { exact: true })).toBeVisible();
  // A row that cannot take the prompt is not offered.
  await expect(dialog.getByText('Terminal', { exact: true })).toHaveCount(0);
  await fits();
  await page.screenshot({ path: '.tmp/issues-web-launch.png' });
  await page.goBack();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByTestId('issues-screen')).toBeVisible();
  await page.getByRole('button', { name: 'Edit tags for issue #42', exact: true }).click();
  const checkbox = page.getByRole('checkbox', { name: /buildmesh:run/ });
  await expect(checkbox).toBeVisible();
  await checkbox.click();
  await expect(checkbox).toBeChecked();
  await expect(page.getByRole('checkbox', { name: 'bug', exact: true })).toBeChecked();
  await checkbox.click();
  await expect(checkbox).not.toBeChecked();
  expect(writes).toEqual([{ label: 'buildmesh:run', present: true }, { label: 'buildmesh:run', present: false }]);
  await fits();
  console.log('Mobile triage browser assertions passed; fixture HTTP, 390px phone width.');
}
