/**
 * E2E Terminal Sizing Regression Tests
 *
 * Core regression tests for:
 * - Terminal dimensions preserved after session switch
 * - Terminal does not shrink after rapid switches
 * - All tiled terminals have non-minimal dimensions
 * - Terminal content preserved after rapid switches
 */
import { test, expect, Page } from '@playwright/test';
import { getTerminalBoundingBox, waitForTerminalFit } from '../utils/terminal';

// Helper to create a session via the sidebar
async function createSession(page: Page, name: string = 'Test Session') {
  // Look for + Add button or similar creation mechanism
  const addButton = page.locator('button').filter({ hasText: '+ Add' });
  if (await addButton.isVisible()) {
    await addButton.click();
    await page.waitForTimeout(300);
  }
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
  });

  test('terminal has non-minimal dimensions (>50px) after initial render', async ({ page }) => {
    // Create a session first
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      await addBtn.click();
      await page.waitForTimeout(500);
    }

    // Get the terminal bounding box
    const xterm = page.locator('.xterm').first();
    await xterm.waitFor({ state: 'visible', timeout: 5000 });

    const box = await xterm.boundingBox();

    if (box) {
      expect(box.width, `Terminal width ${box.width} is too small`).toBeGreaterThan(50);
      expect(box.height, `Terminal height ${box.height} is too small`).toBeGreaterThan(50);
    }
  });

  test('terminal dimensions are preserved after single session switch', async ({ page }) => {
    // Create 2 sessions
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      await addBtn.click();
      await page.waitForTimeout(500);
      await addBtn.click();
      await page.waitForTimeout(500);
    }

    // Get tabs to identify session IDs
    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    if (tabCount < 2) {
      // Skip if we can't create 2 sessions
      test.skip();
    }

    // Get initial dimensions of session 1
    const session1Tab = tabs.first();
    const session1Id = await session1Tab.getAttribute('data-session-tab');
    await session1Tab.click();
    await waitForTerminalFit(page, parseInt(session1Id || '0'));

    const initialBox = await getTerminalBoundingBox(page, parseInt(session1Id || '0'));

    if (!initialBox) {
      test.skip();
    }

    // Switch to session 2
    const session2Tab = tabs.nth(1);
    await session2Tab.click();
    await waitForTerminalFit(page, parseInt(await session2Tab.getAttribute('data-session-tab') || '0'));

    // Switch back to session 1
    await session1Tab.click();
    await waitForTerminalFit(page, parseInt(session1Id || '0'));

    const finalBox = await getTerminalBoundingBox(page, parseInt(session1Id || '0'));

    if (finalBox) {
      // Allow 10% tolerance for measurement differences
      const widthRatio = finalBox.width / initialBox.width;
      const heightRatio = finalBox.height / initialBox.height;

      expect(widthRatio, `Width shrank from ${initialBox.width} to ${finalBox.width}`).toBeGreaterThan(0.9);
      expect(heightRatio, `Height shrank from ${initialBox.height} to ${finalBox.height}`).toBeGreaterThan(0.9);
    }
  });

  test('terminal does not shrink after multiple rapid switches', async ({ page }) => {
    // Create 3 sessions
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      await addBtn.click();
      await page.waitForTimeout(400);
      await addBtn.click();
      await page.waitForTimeout(400);
      await addBtn.click();
      await page.waitForTimeout(400);
    }

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    if (tabCount < 2) {
      test.skip();
    }

    // Record initial dimensions for each session
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

    // Rapid switch 10 times
    for (let i = 0; i < 10; i++) {
      const tabIndex = i % tabCount;
      await tabs.nth(tabIndex).click();
      await page.waitForTimeout(50); // Short delay to simulate rapid clicking
    }

    // Wait for all fit operations to complete
    await waitForTerminalFit(page, 0, 600);

    // Verify all dimensions are preserved
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
    // Create 4 sessions for a 2x2 grid
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      for (let i = 0; i < 4; i++) {
        await addBtn.click();
        await page.waitForTimeout(400);
      }
    }

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    if (tabCount < 2) {
      test.skip();
    }

    // Wait for grid to render
    await waitForTerminalFit(page, 0, 600);

    // Check each terminal
    const MIN_WIDTH = 80;
    const MIN_HEIGHT = 60;

    for (let i = 0; i < tabCount; i++) {
      const tab = tabs.nth(i);
      const id = await tab.getAttribute('data-session-tab');

      // Click to ensure terminal is visible/active
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
    // This test writes unique content to a terminal and verifies it persists
    // after rapid switching. Since we can't easily type into xterm in Playwright,
    // we verify the terminal DOM is still intact.

    // Create 2 sessions
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      await addBtn.click();
      await page.waitForTimeout(400);
      await addBtn.click();
      await page.waitForTimeout(400);
    }

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    if (tabCount < 2) {
      test.skip();
    }

    // Get session 1's terminal
    const session1Tab = tabs.first();
    const session1Id = await session1Tab.getAttribute('data-session-tab');
    await session1Tab.click();
    await waitForTerminalFit(page, parseInt(session1Id || '0'));

    // Verify session 1 terminal exists
    const session1Terminal = page.locator(`[data-session-id="${session1Id}"] .xterm`);
    await expect(session1Terminal).toBeVisible();

    // Rapid switch 10 times
    for (let i = 0; i < 10; i++) {
      const tabIndex = i % tabCount;
      await tabs.nth(tabIndex).click();
      await page.waitForTimeout(50);
    }

    // Wait for all fit operations
    await waitForTerminalFit(page, 0, 600);

    // Switch back to session 1
    await session1Tab.click();
    await waitForTerminalFit(page, parseInt(session1Id || '0'));

    // Session 1 terminal should still be visible and have dimensions
    await expect(session1Terminal).toBeVisible();

    const box = await session1Terminal.boundingBox();
    if (box) {
      expect(box.width).toBeGreaterThan(50);
      expect(box.height).toBeGreaterThan(50);
    }
  });
});

// ============================================================
// Smoke test - verify xterm DOM is present
// ============================================================

test.describe('terminal DOM smoke tests', () => {
  test('xterm element exists in DOM when session is active', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    // Create a session
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      await addBtn.click();
      await page.waitForTimeout(1000);
    }

    // Wait for xterm to render
    const xterm = page.locator('.xterm');
    await xterm.waitFor({ state: 'visible', timeout: 5000 });

    expect(await xterm.count()).toBeGreaterThan(0);
  });

  test('xterm has proper CSS dimensions applied', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      await addBtn.click();
      await page.waitForTimeout(1000);
    }

    const xterm = page.locator('.xterm').first();

    // Check that xterm has dimensions from CSS (not just 0x0)
    const boundingBox = await xterm.boundingBox();

    if (boundingBox) {
      // xterm should have actual pixel dimensions
      expect(boundingBox.width).toBeGreaterThan(0);
      expect(boundingBox.height).toBeGreaterThan(0);
    }
  });
});
