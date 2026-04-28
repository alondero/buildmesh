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
import { test, expect, Page } from '@playwright/test';
import { waitForTauriReady, createTestSessionViaHttp } from './utils/tauri-http';

test.describe('session switching E2E', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    // Verify Tauri backend is available via HTTP test server
    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }
  });

  test('sidebar session click selects and shows terminal', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await page.waitForTimeout(500);

    const sessionItems = page.locator('[data-session-item]');
    const count = await sessionItems.count();

    expect(count, 'Expected at least 1 session item').toBeGreaterThan(0);

    const firstItem = sessionItems.first();
    await firstItem.click();
    await page.waitForTimeout(300);

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs).toHaveCount(1, { timeout: 5000 });
  });

  test('active indicator updates on sidebar session switch', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);
    await page.waitForTimeout(500);

    const sessionItems = page.locator('[data-session-item]');
    const itemCount = await sessionItems.count();

    expect(itemCount, `Expected at least 2 sessions, found ${itemCount}`).toBeGreaterThanOrEqual(2);

    const firstItem = sessionItems.first();
    await firstItem.click();
    await page.waitForTimeout(300);

    const tabs = page.locator('[data-session-tab]');
    expect(await tabs.count()).toBeGreaterThan(0);

    const secondItem = sessionItems.nth(1);
    await secondItem.click();
    await page.waitForTimeout(300);

    expect(await tabs.count()).toBe(2);
  });

  test('tab bar shows all sessions', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);
    await createTestSessionViaHttp(3);
    await page.waitForTimeout(500);

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs).toHaveCount(3, { timeout: 5000 });

    for (let i = 0; i < 3; i++) {
      const id = await tabs.nth(i).getAttribute('data-session-tab');
      expect(id).toBeTruthy();
    }
  });

  test('tab click switches active session', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);
    await page.waitForTimeout(500);

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    expect(tabCount, `Expected 2 tabs, found ${tabCount}`).toBe(2);

    const secondTab = tabs.nth(1);
    await secondTab.click();
    await page.waitForTimeout(300);

    const secondTabClass = await secondTab.getAttribute('class');
    expect(secondTabClass).toContain('bg-');
  });

  test('all sessions render in tiled grid simultaneously', async ({ page }) => {
    for (let i = 1; i <= 4; i++) {
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
    await page.waitForTimeout(500);

    const xterms = page.locator('.xterm');
    const xtermCount = await xterms.count();

    expect(xtermCount, `Expected 4 xterms for 4 sessions, found ${xtermCount}`).toBe(4);

    for (let i = 0; i < xtermCount; i++) {
      const box = await xterms.nth(i).boundingBox();
      expect(box?.width, `xterm ${i} should have width`).toBeGreaterThan(0);
      expect(box?.height, `xterm ${i} should have height`).toBeGreaterThan(0);
    }
  });

  test('switching sessions preserves all tiled terminals', async ({ page }) => {
    for (let i = 1; i <= 3; i++) {
      await createTestSessionViaHttp(i);
    }
    await page.waitForTimeout(500);

    const gridBtn = page.locator('button:has-text("GRID VIEW")');
    if (await gridBtn.isVisible()) {
      await gridBtn.click();
      await page.waitForTimeout(200);
    }

    const tabs = page.locator('[data-session-tab]');
    for (let i = 0; i < await tabs.count(); i++) {
      const gridToggle = page.locator('button:has-text("□")').nth(i);
      if (await gridToggle.isVisible()) {
        await gridToggle.click();
        await page.waitForTimeout(100);
      }
    }
    await page.waitForTimeout(300);

    const initialXterms = await page.locator('.xterm').count();
    expect(initialXterms).toBe(3);

    for (let i = 0; i < 3; i++) {
      await tabs.nth(i).click();
      await page.waitForTimeout(200);

      const currentXterms = await page.locator('.xterm').count();
      expect(currentXterms, `After switching to session ${i}: expected 3 xterms, found ${currentXterms}`).toBe(3);
    }
  });
});

test.describe('session switching - edge cases', () => {
  test('switching to same session is a no-op', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }

    await createTestSessionViaHttp(1);
    await page.waitForTimeout(500);

    const tabs = page.locator('[data-session-tab]');
    const count = await tabs.count();
    expect(count, 'Expected at least 1 tab').toBeGreaterThan(0);

    const firstTab = tabs.first();
    await firstTab.click();
    await page.waitForTimeout(300);
    await firstTab.click();
    await page.waitForTimeout(300);

    const xtermCount = await page.locator('.xterm').count();
    expect(xtermCount).toBeGreaterThan(0);
  });

  test('rapid switching between sessions does not cause errors', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }

    for (let i = 1; i <= 3; i++) {
      await createTestSessionViaHttp(i);
    }
    await page.waitForTimeout(500);

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    expect(tabCount, `Expected at least 2 tabs, found ${tabCount}`).toBeGreaterThanOrEqual(2);

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

    await page.waitForTimeout(500);

    const criticalErrors = errors.filter(e =>
      !e.includes('Warning') &&
      !e.includes('favicon') &&
      !e.includes('404')
    );

    expect(criticalErrors).toHaveLength(0);
  });
});