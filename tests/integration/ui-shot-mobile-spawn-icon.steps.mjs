/**
 * Browser check: the default provider's brand mark on the phone's spawn split
 * button.
 *
 * The desktop's spawn cluster draws the resolved default harness's brand mark
 * on its quick-spawn half (issue #2040); the phone now draws the same mark
 * beside its label. This run drives the real mobile SPA in Chromium at phone
 * width with fixture HTTP, so the mark is proved on the surfaces the user
 * actually touches: the mesh card's **New agent** button and the issue card's
 * split button.
 *
 * Two meshes stand in for the two states the mark can be in. The mesh that
 * pins a default shows that default's brand mark; the mesh that pins no
 * default shows no mark at all, because an app-wide default is invisible to
 * the phone and a wrong mark would be worse than none.
 *
 * Mock IPC only — this proves the phone's rendering, not the backend route.
 * Run it with:
 *   node scripts/ui-shot.mjs --mock --serve --mock-url http://127.0.0.1:3999 \
 *     --out .tmp/mobile-spawn-icon.png --viewport 390x844 \
 *     --steps tests/integration/ui-shot-mobile-spawn-icon.steps.mjs
 */
import { expect } from '@playwright/test';

export default async function ({ page }) {
  const pinnedMesh = {
    id: 3,
    name: 'Phone triage',
    path: '/repo',
    created_at: '',
    scratchpad: '',
    sandbox: false,
    default_provider: 'claude',
  };
  const unpinnedMesh = {
    id: 4,
    name: 'Phone plain',
    path: '/repo',
    created_at: '',
    scratchpad: '',
    sandbox: false,
    default_provider: null,
  };
  const issue = {
    number: 42,
    title: 'Fix the mobile deadline',
    body: 'Expanded issue details',
    url: 'https://github.com/o/r/issues/42',
    state: 'open',
    labels: [],
    blocked_by: [],
    author: 'octo',
  };
  const providers = [
    {
      id: 'claude',
      label: 'Claude Code',
      harness_id: 'claude',
      provider_id: null,
      icon: 'A',
      color: '#1d7cfc',
      capabilities: { supports_prefill: true },
    },
    {
      id: 'terminal',
      label: 'Terminal',
      harness_id: 'terminal',
      provider_id: null,
      icon: 'T',
      color: '#9ca3af',
      capabilities: { supports_prefill: false },
    },
  ];

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    let body;
    if (path === '/api/session') return route.fulfill({ status: 204 });
    if (path === '/api/meshes') body = [pinnedMesh, unpinnedMesh];
    else if (path === '/api/nodes') body = [];
    else if (path === '/api/providers') body = providers;
    else if (path === '/api/meshes/3/issues') body = {
      items: [issue],
      completeness: { returned: 1, pages_fetched: 1, complete: true, incomplete_reason: null, reported_total: 1 },
    };
    else if (path === '/api/meshes/3/issue-labels') body = { labels: [], autopilot_triggers: {} };
    else if (path === '/api/circuit-runs') body = [];
    // A ticket mint needs a live socket, which this run has no backend for.
    // 204 is a success status, so the browser logs no resource error, and the
    // empty body fails the mint before any socket is opened.
    else if (path === '/api/ws-ticket') return route.fulfill({ status: 204, body: '' });
    else { console.log('unmocked endpoint', path); return route.fulfill({ status: 503, json: { error: 'Fixture endpoint unavailable' } }); }
    await route.fulfill({ json: body });
  });

  await page.goto(`${new URL(page.url()).origin}/mobile/index.html`, { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: 'Work', exact: true }).click();
  // The mesh card's quick-spawn half: the pinned default's brand mark beside
  // the label, and the mark is the registered brand rather than a grey dot.
  const meshSpawn = page.getByTestId('mesh-spawn-3');
  await expect(meshSpawn).toBeVisible();
  await expect(meshSpawn.locator('.spawn-split-brand')).toBeVisible();
  await expect(meshSpawn.locator('svg.spawn-split-brand')).toHaveCount(1);
  await expect(meshSpawn).toContainText('New agent');
  const mark = await meshSpawn.locator('svg.spawn-split-brand').boundingBox();
  expect(mark.width).toBe(16);
  expect(mark.height).toBe(16);

  // A mesh that pins no default shows no mark: the phone does not see the
  // app-wide default, so it must not guess one.
  const plainSpawn = page.getByTestId('mesh-spawn-4');
  await expect(plainSpawn).toBeVisible();
  await expect(plainSpawn.locator('.spawn-split-icon')).toHaveCount(0);

  await page.screenshot({ path: '.tmp/mobile-spawn-icon-mesh.png' });

  // The issue card's split button: the same mark, beside the default's name.
  // The button belongs to the expanded card, so the card is opened first.
  await page.getByTestId('mesh-actions-3').click();
  await page.getByTestId('mesh-sheet-issues').click();
  await expect(page.getByTestId('issue-42')).toBeVisible();
  // The card's own centre lands on the nested "Edit tags" button, which stops
  // the click from expanding the card, so the title is what gets clicked.
  await page.getByText('Fix the mobile deadline', { exact: true }).click();
  await expect(page.getByTestId('issue-body-42')).toBeVisible();
  const issueSpawn = page.getByTestId('issue-spawn-42');
  await expect(issueSpawn).toBeVisible();
  await expect(issueSpawn.locator('svg.spawn-split-brand')).toHaveCount(1);
  await expect(issueSpawn).toContainText('Claude Code');

  // The button still fits the phone's width with the mark added.
  const fits = await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
  expect(fits).toBe(true);

  await page.screenshot({ path: '.tmp/mobile-spawn-icon-issue.png' });
  console.log('Mobile spawn brand mark rendered on the mesh and issue cards at 390px.');
}
