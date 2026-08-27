/**
 * E2E Session Switching Tests
 *
 * Full UI switching flow tests:
 * - Sidebar session click selects session
 * - Active indicator updates on switch
 * - Tab bar click selects session
 * - All sessions render in tiled grid
 *
 * Uses HTTP test server on port 1991 to call Tauri commands instead of
 * window.__TAURI__. This works because Playwright connects via HTTP to the
 * Vite dev server, but invoke() requires Tauri webview context.
 */
import { test, expect } from '@playwright/test';
import { waitForTauriReady, createTestSessionViaHttp, cleanupTestProjects } from './utils/tauri-http';
import { waitForAppBoot } from './utils/state-waits';

test.describe('session switching E2E', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/');

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
      return;
    }

    await waitForAppBoot(page);
  });

  test.afterEach(async () => {
    await cleanupTestProjects();
  });

  test('sidebar session click selects and shows terminal', async ({ page }) => {
    await createTestSessionViaHttp(1);

    const sessionItems = page.locator('[data-session-item]');
    await expect(sessionItems, 'first session row should appear after WS push').toHaveCount(1, { timeout: 10000 });

    await sessionItems.first().click();

    await expect(page.locator('[data-session-tab]'), 'tab bar should hold 1 tab after clicking the session').toHaveCount(1, { timeout: 10000 });
  });

  test('active indicator updates on sidebar session switch', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);

    const sessionItems = page.locator('[data-session-item]');
    await expect(sessionItems, 'sidebar should hold 2 session rows').toHaveCount(2, { timeout: 10000 });

    await sessionItems.first().click();
    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'first tab should appear after clicking session 1').toHaveCount(1, { timeout: 5000 });

    await sessionItems.nth(1).click();
    await expect(tabs, 'tab bar should hold 2 tabs after both sessions are clicked').toHaveCount(2, { timeout: 10000 });
  });

  test('tab bar shows all sessions', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);
    await createTestSessionViaHttp(3);

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 3 tabs after creating 3 sessions').toHaveCount(3, { timeout: 10000 });

    for (let i = 0; i < 3; i++) {
      const id = await tabs.nth(i).getAttribute('data-session-tab');
      expect(id).toBeTruthy();
    }
  });

  test('tab click switches active session', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 2 tabs after creating 2 sessions').toHaveCount(2, { timeout: 10000 });

    const secondTab = tabs.nth(1);
    await secondTab.click();

    await expect(secondTab, 'second tab should carry the active-tab class after click').toHaveClass(/bg-/, { timeout: 5000 });
  });

  test('all sessions render in tiled grid simultaneously', async ({ page }) => {
    for (let i = 1; i <= 4; i++) {
      await createTestSessionViaHttp(i);
    }

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 4 tabs after creating 4 sessions').toHaveCount(4, { timeout: 10000 });
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

    const xterms = page.locator('.xterm');
    await expect(xterms, `tiled grid should mount ${tabCount} xterms`).toHaveCount(tabCount, { timeout: 10000 });

    for (let i = 0; i < tabCount; i++) {
      const box = await xterms.nth(i).boundingBox();
      expect(box?.width, `xterm ${i} should have width`).toBeGreaterThan(0);
      expect(box?.height, `xterm ${i} should have height`).toBeGreaterThan(0);
    }
  });

  test('switching sessions preserves all tiled terminals', async ({ page }) => {
    for (let i = 1; i <= 3; i++) {
      await createTestSessionViaHttp(i);
    }

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 3 tabs after creating 3 sessions').toHaveCount(3, { timeout: 10000 });

    const gridBtn = page.locator('button:has-text("GRID VIEW")');
    if (await gridBtn.isVisible()) {
      await gridBtn.click();
    }

    for (let i = 0; i < await tabs.count(); i++) {
      const gridToggle = page.locator('button:has-text("□")').nth(i);
      if (await gridToggle.isVisible()) {
        await gridToggle.click();
      }
    }

    const xterms = page.locator('.xterm');
    await expect(xterms, 'tiled grid should mount 3 xterms').toHaveCount(3, { timeout: 10000 });

    for (let i = 0; i < 3; i++) {
      await tabs.nth(i).click();
      await expect(xterms, `after switching to session ${i}, grid should still hold 3 xterms`).toHaveCount(3, { timeout: 5000 });
    }
  });
});

test.describe('session switching - edge cases', () => {
  test('switching to same session is a no-op', async ({ page }) => {
    await page.goto('/');

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
      return;
    }

    await waitForAppBoot(page);
    await createTestSessionViaHttp(1);

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 1 tab after creating 1 session').toHaveCount(1, { timeout: 10000 });

    const firstTab = tabs.first();
    await firstTab.click();
    await firstTab.click();

    await expect(page.locator('.xterm'), 'xterm should remain mounted after re-clicking the active tab').toHaveCount(1, { timeout: 5000 });
  });

  test('rapid switching between sessions does not cause errors', async ({ page }) => {
    await page.goto('/');

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
      return;
    }

    await waitForAppBoot(page);

    for (let i = 1; i <= 3; i++) {
      await createTestSessionViaHttp(i);
    }

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 3 tabs after creating 3 sessions').toHaveCount(3, { timeout: 10000 });
    const tabCount = await tabs.count();

    const errors: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'error') {
        errors.push(msg.text());
      }
    });

    for (let i = 0; i < 20; i++) {
      const tabIndex = i % tabCount;
      await tabs.nth(tabIndex).click();
    }

    // Brief settle so late console errors (async chunks triggered by
    // the rapid switches) have a chance to flush.
    await page.waitForTimeout(200);

    const criticalErrors = errors.filter(e =>
      !e.includes('Warning') &&
      !e.includes('favicon') &&
      !e.includes('404')
    );

    expect(criticalErrors).toHaveLength(0);
  });
});
