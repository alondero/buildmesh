import { test, expect } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const USAGE_FIXTURES = {
  ...defaultFixtures,
  get_provider_accounts: [
    {
      id: 'anthropic',
      name: 'Anthropic / Claude',
      enabled: true,
      billing_mode: 'plan',
      claude_compatible: false,
      api_key: null,
    },
  ],
  get_provider_meters: [
    {
      provider: 'anthropic',
      usageTracked: true,
      usage: {
        provider: 'anthropic',
        loggedIn: true,
        windows: [],
        balance: null,
        meters: [
          {
            state: 'metered',
            amount: {
              used: 25,
              limit: 100,
              remaining: 75,
              unit: 'USD',
              usedPercent: 25,
              resetsAt: '2026-10-01T00:00:00Z',
            },
          },
          {
            state: 'no_individual_limit',
            amount: {
              used: 12,
              limit: null,
              remaining: null,
              unit: 'USD',
              usedPercent: null,
              resetsAt: null,
            },
          },
          { state: 'unlimited' },
          {
            state: 'managed_externally',
            platform: 'A deliberately long AWS Bedrock billing source label',
          },
          { state: 'unavailable' },
        ],
        detail: null,
        error: null,
      },
    },
  ],
};

test.describe('explicit Usage Meter states', () => {
  for (const panelWidth of [360, 240]) {
    test(`renders every state without horizontal overflow at ${panelWidth}px`, async ({ page }) => {
      await page.addInitScript({ content: buildInitScript(USAGE_FIXTURES) });
      await page.addInitScript((width) => {
        window.localStorage.setItem('buildmesh.probe-panel-width', String(width));
      }, panelWidth);

      await page.goto('/');
      await page.getByTestId('titlebar-usage').click();

      const panel = page.getByTestId('usage-panel-anthropic');
      await expect(panel).toBeVisible();
      // Plan label is suppressed on the glanceable Usage Meter (informational noise at this surface).
      await expect(panel.getByText(/Plan:/)).toHaveCount(0);
      await expect(panel.getByText('USD 25.00')).toBeVisible();
      await expect(panel.getByText('USD 100.00')).toBeVisible();
      await expect(panel.getByText('USD 75.00')).toBeVisible();
      await expect(panel.getByText('25.0%')).toBeVisible();
      await expect(panel.getByText('USD 12.00')).toBeVisible();
      await expect(panel.getByText('No individual limit')).toBeVisible();
      await expect(panel.getByText('Unlimited')).toBeVisible();
      await expect(panel.getByText(/Managed by A deliberately long AWS Bedrock/)).toBeVisible();
      await expect(panel.getByText('Unavailable')).toBeVisible();

      const probe = page.getByRole('region', { name: 'Probe panel' });
      await expect.poll(async () => probe.evaluate((element) => element.getBoundingClientRect().width))
        .toBe(panelWidth);
      expect(await probe.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
    });
  }
});
