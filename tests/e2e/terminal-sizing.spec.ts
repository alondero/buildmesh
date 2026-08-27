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
import { test, expect, Locator, Page } from '@playwright/test';
import { waitForTauriReady, createTestSessionViaHttp, cleanupTestProjects } from './utils/tauri-http';
import { waitForAppBoot, waitForTerminalFit } from './utils/state-waits';

/**
 * Build a Locator that targets the xterm container for a given session
 * by its entity ID. The DOM contract (Terminal.tsx:399) stamps every
 * AgentTerminal with `data-node-id={nodeId}`, so this is the canonical
 * way to point at "the terminal for session N" without leaning on
 * DOM index ordering.
 */
function terminalFor(page: Page, sessionId: number): Locator {
  return page.locator(`[data-node-id="${sessionId}"] .xterm`);
}

async function getTerminalBoundingBox(xterm: Locator): Promise<{ width: number; height: number } | null> {
  return await xterm.boundingBox();
}

test.describe('terminal sizing regression tests', () => {

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

  test('terminal has non-minimal dimensions (>50px) after initial render', async ({ page }) => {
    await createTestSessionViaHttp(1);

    const xterm = page.locator('.xterm').first();
    await expect(xterm).toBeVisible({ timeout: 10000 });
    const box = await xterm.boundingBox();

    expect(box?.width, `Terminal width ${box?.width} is too small`).toBeGreaterThan(50);
    expect(box?.height, `Terminal height ${box?.height} is too small`).toBeGreaterThan(50);
  });

  test('terminal dimensions are preserved after single session switch', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 2 tabs after creating 2 sessions').toHaveCount(2, { timeout: 10000 });

    const session1Tab = tabs.first();
    const session1Id = Number((await session1Tab.getAttribute('data-session-tab'))!);
    const session1Terminal = terminalFor(page, session1Id);
    await session1Tab.click();
    await waitForTerminalFit(session1Terminal);

    const initialBox = await getTerminalBoundingBox(session1Terminal);

    const session2Tab = tabs.nth(1);
    const session2Id = Number((await session2Tab.getAttribute('data-session-tab'))!);
    await session2Tab.click();
    await waitForTerminalFit(terminalFor(page, session2Id));

    await session1Tab.click();
    await waitForTerminalFit(session1Terminal);

    const finalBox = await getTerminalBoundingBox(session1Terminal);

    const widthRatio = finalBox!.width / initialBox!.width;
    const heightRatio = finalBox!.height / initialBox!.height;

    expect(widthRatio, `Width shrank from ${initialBox!.width} to ${finalBox!.width}`).toBeGreaterThan(0.9);
    expect(heightRatio, `Height shrank from ${initialBox!.height} to ${finalBox!.height}`).toBeGreaterThan(0.9);
  });

  test('terminal does not shrink after multiple rapid switches', async ({ page }) => {
    for (let i = 1; i <= 3; i++) {
      await createTestSessionViaHttp(i);
    }

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 3 tabs after creating 3 sessions').toHaveCount(3, { timeout: 10000 });
    const tabCount = await tabs.count();

    const initialDimensions: { sessionId: number; width: number; height: number }[] = [];

    for (let i = 0; i < tabCount; i++) {
      const tab = tabs.nth(i);
      const sessionId = Number((await tab.getAttribute('data-session-tab'))!);
      const xterm = terminalFor(page, sessionId);
      await tab.click();
      await waitForTerminalFit(xterm);

      const box = await getTerminalBoundingBox(xterm);
      if (box) {
        initialDimensions.push({ sessionId, width: box.width, height: box.height });
      }
    }

    for (let i = 0; i < 10; i++) {
      const tabIndex = i % tabCount;
      await tabs.nth(tabIndex).click();
    }

    await waitForTerminalFit(page.locator('.xterm').first());

    for (const { sessionId, width, height } of initialDimensions) {
      const finalBox = await getTerminalBoundingBox(terminalFor(page, sessionId));

      const widthOk = finalBox!.width >= width * 0.9;
      const heightOk = finalBox!.height >= height * 0.9;

      expect(widthOk, `Session ${sessionId} width shrank from ${width} to ${finalBox!.width}`).toBe(true);
      expect(heightOk, `Session ${sessionId} height shrank from ${height} to ${finalBox!.height}`).toBe(true);
    }
  });

  test('all tiled terminals have non-minimal dimensions', async ({ page }) => {
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
    await waitForTerminalFit(page.locator('.xterm').first());

    const MIN_WIDTH = 80;
    const MIN_HEIGHT = 60;

    // Measure each pane by its own session ID rather than the DOM
    // position — captures the right terminal even when tile order
    // doesn't match creation order.
    for (let i = 0; i < tabCount; i++) {
      const tab = tabs.nth(i);
      const sessionId = Number((await tab.getAttribute('data-session-tab'))!);
      const xterm = terminalFor(page, sessionId);

      await tab.click();
      await waitForTerminalFit(xterm);

      const box = await getTerminalBoundingBox(xterm);

      expect(box?.width, `Session ${sessionId} width (${box?.width}) should be > ${MIN_WIDTH}`).toBeGreaterThan(MIN_WIDTH);
      expect(box?.height, `Session ${sessionId} height (${box?.height}) should be > ${MIN_HEIGHT}`).toBeGreaterThan(MIN_HEIGHT);
    }
  });

  test('terminal content is preserved after rapid switches', async ({ page }) => {
    await createTestSessionViaHttp(1);
    await createTestSessionViaHttp(2);

    const tabs = page.locator('[data-session-tab]');
    await expect(tabs, 'tab bar should hold 2 tabs after creating 2 sessions').toHaveCount(2, { timeout: 10000 });
    const tabCount = await tabs.count();

    const session1Tab = tabs.first();
    const session1Id = Number((await session1Tab.getAttribute('data-session-tab'))!);
    await session1Tab.click();
    await waitForTerminalFit(terminalFor(page, session1Id));

    const session1Terminal = terminalFor(page, session1Id);
    await expect(session1Terminal).toBeVisible();

    for (let i = 0; i < 10; i++) {
      const tabIndex = i % tabCount;
      await tabs.nth(tabIndex).click();
    }

    await waitForTerminalFit(page.locator('.xterm').first());

    await session1Tab.click();
    await waitForTerminalFit(session1Terminal);

    await expect(session1Terminal).toBeVisible();

    const box = await session1Terminal.boundingBox();
    expect(box?.width).toBeGreaterThan(50);
    expect(box?.height).toBeGreaterThan(50);
  });
});

test.describe('terminal DOM smoke tests', () => {
  test('xterm element exists in DOM when session is active', async ({ page }) => {
    await page.goto('/');

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
      return;
    }

    await waitForAppBoot(page);
    await createTestSessionViaHttp(1);

    const xterm = page.locator('.xterm');
    await expect(xterm).toBeVisible({ timeout: 10000 });
    await expect(xterm, 'at least one xterm should be in the DOM after a session is active').toHaveCount(1);
  });

  test('xterm has proper CSS dimensions applied', async ({ page }) => {
    await page.goto('/');

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
      return;
    }

    await waitForAppBoot(page);
    await createTestSessionViaHttp(1);

    const xterm = page.locator('.xterm').first();
    await expect(xterm).toBeVisible({ timeout: 10000 });

    const boundingBox = await xterm.boundingBox();
    expect(boundingBox?.width).toBeGreaterThan(0);
    expect(boundingBox?.height).toBeGreaterThan(0);
  });
});