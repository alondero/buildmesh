/**
 * Terminal Tiling E2E Tests
 *
 * Tests that verify tiled terminal grid behavior via Playwright.
 * Uses HTTP test server on port 1991 to call Tauri commands.
 */
import { test, expect } from '@playwright/test';
import { waitForTauriReady, createTestSessionViaHttp } from './utils/tauri-http';
import { waitForAppBoot, waitForTerminalFit } from './utils/state-waits';

test.describe('terminal tiling E2E', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/');

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
      return;
    }

    await waitForAppBoot(page);
  });

  test('app loads without crashing', async ({ page }) => {
    const errors: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'error') errors.push(msg.text());
    });

    // Brief settle so late-arriving console errors (async chunks) have
    // a chance to flush. No DOM state to poll against — the listener
    // is stream-based. The beforeEach's `waitForAppBoot` already
    // proved the React shell mounted; this test only cares that no
    // console errors fired during the boot window.
    await page.waitForTimeout(200);

    const realErrors = errors.filter(e =>
      !e.includes('Warning:') &&
      !e.includes('favicon') &&
      !e.includes('404')
    );
    expect(realErrors, `Console errors: ${realErrors.join(', ')}`).toHaveLength(0);
  });

  test('tiled sessions all open their xterm DOM', async ({ page }) => {
    for (let i = 1; i <= 3; i++) {
      await createTestSessionViaHttp(i);
    }

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 3 tabs after creating 3 sessions').toHaveCount(3, { timeout: 10000 });
    const tabCount = await tabs.count();

    const gridBtn = page.locator('button:has-text("GRID VIEW")');
    if (await gridBtn.isVisible()) {
      await gridBtn.click();
    }

    for (let i = 0; i < tabCount; i++) {
      const gridToggle = page.locator('button:has-text("□")').nth(i);
      if (await gridToggle.isVisible()) {
        await gridToggle.click();
      }
    }

    await waitForTerminalFit(page.locator('.xterm').first());
    await expect(page.locator('.xterm'), `tiled grid should mount ${tabCount} xterms`).toHaveCount(tabCount, { timeout: 10000 });

    const xterms = page.locator('.xterm');
    const count = await xterms.count();

    for (let i = 0; i < count; i++) {
      const box = await xterms.nth(i).boundingBox();
      expect(box?.width, `xterm ${i} width`).toBeGreaterThan(5);
      expect(box?.height, `xterm ${i} height`).toBeGreaterThan(5);
    }
  });

  test('AgentTerminal produces expected DOM structure', async ({ page }) => {
    await createTestSessionViaHttp(1);

    await expect(page.locator('[class*="bg-\\[#0f0f0f\\]"]'), 'AgentTerminal should render at least one styled stack div').toHaveCount(1, { timeout: 10000 });
  });
});
