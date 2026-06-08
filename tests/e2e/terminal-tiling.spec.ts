/**
 * Terminal Tiling E2E Tests
 *
 * Tests that verify tiled terminal grid behavior via Playwright.
 * Uses HTTP test server on port 1991 to call Tauri commands.
 */
import { test, expect, Page } from '@playwright/test';
import { waitForTauriReady, createTestSessionViaHttp } from './utils/tauri-http';

async function waitForTerminalFit(page: Page, _sessionId: number, timeout = 300): Promise<void> {
  await page.waitForTimeout(timeout);
}

test.describe('terminal tiling E2E', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }
  });

  test('app loads without crashing', async ({ page }) => {
    const errors: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'error') errors.push(msg.text());
    });

    await page.waitForTimeout(2000);

    const body = await page.locator('body').innerHTML();
    expect(body.length, 'Page body is empty - app may have crashed').toBeGreaterThan(100);

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
    await page.waitForTimeout(500);

    const gridBtn = page.locator('button:has-text("GRID VIEW")');
    if (await gridBtn.isVisible()) {
      await gridBtn.click();
      await page.waitForTimeout(300);
    }

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    for (let i = 0; i < tabCount; i++) {
      const gridToggle = page.locator('button:has-text("□")').nth(i);
      if (await gridToggle.isVisible()) {
        await gridToggle.click();
        await page.waitForTimeout(100);
      }
    }

    await waitForTerminalFit(page, 0, 600);

    const xterms = page.locator('.xterm');
    const count = await xterms.count();

    expect(count, `Expected ${tabCount} xterms in tiled grid, found ${count}`).toBe(tabCount);

    for (let i = 0; i < count; i++) {
      const box = await xterms.nth(i).boundingBox();
      expect(box?.width, `xterm ${i} width`).toBeGreaterThan(5);
      expect(box?.height, `xterm ${i} height`).toBeGreaterThan(5);
    }
  });

  test('AgentTerminal produces expected DOM structure', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await page.waitForTimeout(500);

    const terminalStacks = page.locator('[class*="bg-\\[#0f0f0f\\]"]');
    const count = await terminalStacks.count();

    expect(count, `Expected at least 1 terminal stack, found ${count}`).toBeGreaterThan(0);
  });
});