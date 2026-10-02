import { test, expect } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const usagePage = { url: 'https://claude.ai/settings/usage', label: 'View usage' };
const fixtures = {
  ...defaultFixtures,
  get_provider_accounts: [{
    id: 'anthropic',
    name: 'A long Claude account name for a cached usage reading',
    enabled: true,
    billing_mode: 'plan',
    claude_compatible: false,
    api_key: null,
  }],
  get_provider_meters: [{
    provider: 'anthropic',
    usageTracked: true,
    usagePage,
    cachedAt: 1_790_000_000,
    usage: {
      provider: 'anthropic', loggedIn: true,
      windows: [{ label: '5-hour', usedPercent: 41, resetsAt: null }],
      balance: null, meters: [], detail: null, error: null,
    },
  }],
};

test('provider usage link fits a 240px dock and opens externally from a cached or failed reading', async ({ page }) => {
  await page.addInitScript({ content: buildInitScript(fixtures) });
  await page.addInitScript(() => {
    window.localStorage.setItem('buildmesh.probe-panel-width', '240');
  });
  await page.goto('/');
  await page.getByTestId('titlebar-usage').click();
  const panel = page.getByTestId('usage-panel-anthropic');
  const link = panel.getByRole('link', { name: /View usage for/ });
  await expect(link).toBeVisible();
  await expect(link).toHaveAttribute('href', usagePage.url);
  await expect(panel.getByTestId('usage-cached-badge')).toBeVisible();
  await expect(panel.getByText('41.0%')).toBeVisible();

  const probe = page.getByRole('region', { name: 'Probe panel' });
  await expect.poll(() => probe.evaluate(element => element.getBoundingClientRect().width)).toBe(240);
  const panelBounds = await panel.boundingBox();
  const linkBounds = await link.boundingBox();
  expect(panelBounds).not.toBeNull();
  expect(linkBounds).not.toBeNull();
  expect(linkBounds!.x).toBeGreaterThanOrEqual(panelBounds!.x);
  expect(linkBounds!.x + linkBounds!.width).toBeLessThanOrEqual(panelBounds!.x + panelBounds!.width);
  expect(await probe.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);

  const externalOpen = page.waitForEvent('console', message => message.text() === `usage-page-open ${usagePage.url}`);
  await page.evaluate(() => {
    const mock = (window as unknown as { __BUILDMESH_MOCK__: { on: (command: string, handler: (args: { url: string }) => void) => void } }).__BUILDMESH_MOCK__;
    mock.on('plugin:opener|open_url', args => console.debug(`usage-page-open ${args.url}`));
  });
  await link.focus();
  await page.keyboard.press('Enter');
  await externalOpen;
  await expect(panel).toBeVisible();

  await page.evaluate((rows) => {
    const mock = (window as unknown as { __BUILDMESH_MOCK__: { on: (command: string, rows: unknown) => void } }).__BUILDMESH_MOCK__;
    mock.on('get_provider_meters', rows);
  }, [{ ...fixtures.get_provider_meters[0], cachedAt: null, usage: { ...fixtures.get_provider_meters[0].usage, error: 'Unable to reach provider' } }]);
  await page.getByRole('button', { name: 'Refresh usage' }).click();
  await expect(panel.getByText('Unable to reach provider')).toBeVisible();
  await expect(link).toBeVisible();
});
