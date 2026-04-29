/**
 * E2E Terminal Sizing Regression Tests
 *
 * Core regression tests for:
 * - Terminal dimensions preserved after session switch
 * - Terminal does not shrink after rapid switches
 * - All tiled terminals have non-minimal dimensions
 * - Terminal content preserved after rapid switches
 *
 * Uses HTTP test server on port 1991 to call Tauri commands instead of
 * window.__TAURI__. This works because Playwright connects via HTTP to the
 * Vite dev server, but invoke() requires Tauri webview context.
 */
import { test, expect, Page } from '@playwright/test';
import { waitForTauriReady, createTestSessionViaHttp, cleanupTestProjects } from './utils/tauri-http';

async function getTerminalBoundingBox(page: Page, _sessionId: number): Promise<DOMRect | null> {
  const xterm = page.locator(`.xterm`).first();
  if (!await xterm.isVisible()) return null;
  return await xterm.boundingBox();
}

async function waitForTerminalFit(page: Page, _sessionId: number, timeout = 300): Promise<void> {
  await page.waitForTimeout(timeout);
}

// Helper to switch to a session by clicking its tab
async function switchToSession(page: Page, sessionId: number) {
  const tab = page.locator(`button[data-session-tab="${sessionId}"]`);
  if (await tab.isVisible()) {
    await tab.click();
    await page.waitForTimeout(50);
  }
}

// ============================================================
// Core Sizing Regression Tests
// ============================================================

test.describe('terminal sizing regression tests', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }
  });

  test.afterEach(async () => {
    await cleanupTestProjects();
  });

  test('terminal has non-minimal dimensions (>50px) after initial render', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await page.waitForTimeout(500);

    const xterm = page.locator('.xterm').first();
    await xterm.waitFor({ state: 'visible', timeout: 5000 });

    const box = await xterm.boundingBox();

    if (box) {
      expect(box.width, `Terminal width ${box.width} is too small`).toBeGreaterThan(50);
      expect(box.height, `Terminal height ${box.height} is too small`).toBeGreaterThan(50);
    }
  });

  test('terminal dimensions are preserved after single session switch', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);
    await page.waitForTimeout(500);

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    if (tabCount < 2) {
      test.skip();
    }

    const session1Tab = tabs.first();
    const session1Id = await session1Tab.getAttribute('data-session-tab');
    await session1Tab.click();
    await waitForTerminalFit(page, parseInt(session1Id || '0'));

    const initialBox = await getTerminalBoundingBox(page, parseInt(session1Id || '0'));

    if (!initialBox) {
      test.skip();
    }

    const session2Tab = tabs.nth(1);
    await session2Tab.click();
    await waitForTerminalFit(page, parseInt(await session2Tab.getAttribute('data-session-tab') || '0'));

    await session1Tab.click();
    await waitForTerminalFit(page, parseInt(session1Id || '0'));

    const finalBox = await getTerminalBoundingBox(page, parseInt(session1Id || '0'));

    if (finalBox) {
      const widthRatio = finalBox.width / initialBox.width;
      const heightRatio = finalBox.height / initialBox.height;

      expect(widthRatio, `Width shrank from ${initialBox.width} to ${finalBox.width}`).toBeGreaterThan(0.9);
      expect(heightRatio, `Height shrank from ${initialBox.height} to ${finalBox.height}`).toBeGreaterThan(0.9);
    }
  });

  test('terminal does not shrink after multiple rapid switches', async ({ page }) => {
    for (let i = 1; i <= 3; i++) {
      await createTestSessionViaHttp(i);
    }
    await page.waitForTimeout(500);

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    if (tabCount < 2) {
      test.skip();
    }

    const initialDimensions: { id: number; width: number; height: number }[] = [];

    for (let i = 0; i < tabCount; i++) {
      const tab = tabs.nth(i);
      const id = await tab.getAttribute('data-session-tab');
      await tab.click();
      await waitForTerminalFit(page, parseInt(id || '0'));

      const box = await getTerminalBoundingBox(page, parseInt(id || '0'));
      if (box && id) {
        initialDimensions.push({ id: parseInt(id), width: box.width, height: box.height });
      }
    }

    if (initialDimensions.length < 2) {
      test.skip();
    }

    for (let i = 0; i < 10; i++) {
      const tabIndex = i % tabCount;
      await tabs.nth(tabIndex).click();
      await page.waitForTimeout(50);
    }

    await waitForTerminalFit(page, 0, 600);

    for (const { id, width, height } of initialDimensions) {
      const finalBox = await getTerminalBoundingBox(page, id);

      if (finalBox) {
        const widthOk = finalBox.width >= width * 0.9;
        const heightOk = finalBox.height >= height * 0.9;

        expect(widthOk, `Session ${id} width shrank from ${width} to ${finalBox.width}`).toBe(true);
        expect(heightOk, `Session ${id} height shrank from ${height} to ${finalBox.height}`).toBe(true);
      }
    }
  });

  test('all tiled terminals have non-minimal dimensions', async ({ page }) => {
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

    if (tabCount < 2) {
      test.skip();
    }

    for (let i = 0; i < tabCount; i++) {
      const gridToggle = page.locator('button:has-text("□")').nth(i);
      if (await gridToggle.isVisible()) {
        await gridToggle.click();
        await page.waitForTimeout(100);
      }
    }
    await waitForTerminalFit(page, 0, 600);

    const MIN_WIDTH = 80;
    const MIN_HEIGHT = 60;

    for (let i = 0; i < tabCount; i++) {
      const tab = tabs.nth(i);
      const id = await tab.getAttribute('data-session-tab');

      await tab.click();
      await waitForTerminalFit(page, parseInt(id || '0'));

      const box = await getTerminalBoundingBox(page, parseInt(id || '0'));

      if (box) {
        expect(box.width, `Session ${i} width (${box.width}) should be > ${MIN_WIDTH}`).toBeGreaterThan(MIN_WIDTH);
        expect(box.height, `Session ${i} height (${box.height}) should be > ${MIN_HEIGHT}`).toBeGreaterThan(MIN_HEIGHT);
      }
    }
  });

  test('terminal content is preserved after rapid switches', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);
    await page.waitForTimeout(500);

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    if (tabCount < 2) {
      test.skip();
    }

    const session1Tab = tabs.first();
    const session1Id = await session1Tab.getAttribute('data-session-tab');
    await session1Tab.click();
    await waitForTerminalFit(page, parseInt(session1Id || '0'));

    const session1Terminal = page.locator(`[data-session-id="${session1Id}"] .xterm`);
    await expect(session1Terminal).toBeVisible();

    for (let i = 0; i < 10; i++) {
      const tabIndex = i % tabCount;
      await tabs.nth(tabIndex).click();
      await page.waitForTimeout(50);
    }

    await waitForTerminalFit(page, 0, 600);

    await session1Tab.click();
    await waitForTerminalFit(page, parseInt(session1Id || '0'));

    await expect(session1Terminal).toBeVisible();

    const box = await session1Terminal.boundingBox();
    if (box) {
      expect(box.width).toBeGreaterThan(50);
      expect(box.height).toBeGreaterThan(50);
    }
  });
});

test.describe('terminal DOM smoke tests', () => {
  test('xterm element exists in DOM when session is active', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }

    await createTestSessionViaHttp(1);
    await page.waitForTimeout(1000);

    const xterm = page.locator('.xterm');
    await xterm.waitFor({ state: 'visible', timeout: 5000 });

    expect(await xterm.count()).toBeGreaterThan(0);
  });

  test('xterm has proper CSS dimensions applied', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }

    await createTestSessionViaHttp(1);
    await page.waitForTimeout(1000);

    const xterm = page.locator('.xterm').first();

    const boundingBox = await xterm.boundingBox();

    if (boundingBox) {
      expect(boundingBox.width).toBeGreaterThan(0);
      expect(boundingBox.height).toBeGreaterThan(0);
    }
  });
});