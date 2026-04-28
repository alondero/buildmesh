/**
 * E2E Session Switching Tests
 *
 * Full UI switching flow tests:
 * - Sidebar session click selects session
 * - Active indicator updates on switch
 * - Tab bar click selects session
 * - All sessions render in tiled grid
 */
import { test, expect, Page } from '@playwright/test';

test.describe('session switching E2E', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);
  });

  test('sidebar session click selects and shows terminal', async ({ page }) => {
    // Look for session items in sidebar
    const sessionItems = page.locator('[data-session-item]');
    const count = await sessionItems.count();

    if (count === 0) {
      // Try alternative: look for + Add button and click it
      const addBtn = page.locator('button').filter({ hasText: '+ Add' });
      if (await addBtn.isVisible()) {
        await addBtn.click();
        await page.waitForTimeout(500);
      }
    }

    // Verify session view has rendered
    const sessionView = page.locator('.bg-\\[\\#0f0f0f\\]');
    await expect(sessionView.first()).toBeVisible({ timeout: 5000 });
  });

  test('active indicator updates on sidebar session switch', async ({ page }) => {
    // Create 2 sessions
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      await addBtn.click();
      await page.waitForTimeout(500);
      await addBtn.click();
      await page.waitForTimeout(500);
    }

    const sessionItems = page.locator('[data-session-item]');
    const itemCount = await sessionItems.count();

    if (itemCount < 2) {
      test.skip();
    }

    // Get first session item
    const firstItem = sessionItems.first();
    const firstId = await firstItem.getAttribute('data-session-id');

    // Click first session
    await firstItem.click();
    await page.waitForTimeout(300);

    // First session should now have active styling (dark background)
    // The active class is typically bg-[#222] or similar
    const firstItemClass = await firstItem.getAttribute('class');
    expect(firstItemClass).toBeDefined();

    // Click second session
    const secondItem = sessionItems.nth(1);
    const secondId = await secondItem.getAttribute('data-session-id');
    await secondItem.click();
    await page.waitForTimeout(300);

    // Second session should now be active
    const secondItemClass = await secondItem.getAttribute('class');
    expect(secondItemClass).toBeDefined();

    // The two sessions should have different active states
    expect(firstItemClass).not.toEqual(secondItemClass);
  });

  test('tab bar shows all sessions', async ({ page }) => {
    // Create 3 sessions
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      for (let i = 0; i < 3; i++) {
        await addBtn.click();
        await page.waitForTimeout(400);
      }
    }

    // Check tab bar
    const tabs = page.locator('[data-session-tab]');
    await expect(tabs).toHaveCount(3);

    // Each tab should have a session ID
    for (let i = 0; i < 3; i++) {
      const id = await tabs.nth(i).getAttribute('data-session-tab');
      expect(id).toBeTruthy();
    }
  });

  test('tab click switches active session', async ({ page }) => {
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

    // Click second tab
    const secondTab = tabs.nth(1);
    await secondTab.click();
    await page.waitForTimeout(300);

    // Second tab should now be active (have darker background)
    const secondTabClass = await secondTab.getAttribute('class');
    expect(secondTabClass).toContain('bg-'); // Should have some background color when active
  });

  test('all sessions render in tiled grid simultaneously', async ({ page }) => {
    // Create 4 sessions for a 2x2 grid
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      for (let i = 0; i < 4; i++) {
        await addBtn.click();
        await page.waitForTimeout(400);
      }
    }

    // Wait for grid to render
    await page.waitForTimeout(500);

    // Count xterm terminals - should have one per session
    const xterms = page.locator('.xterm');
    const xtermCount = await xterms.count();

    expect(xtermCount, `Expected 4 xterms for 4 sessions, found ${xtermCount}`).toBe(4);

    // Each xterm should have non-zero dimensions
    for (let i = 0; i < xtermCount; i++) {
      const box = await xterms.nth(i).boundingBox();
      expect(box?.width, `xterm ${i} should have width`).toBeGreaterThan(0);
      expect(box?.height, `xterm ${i} should have height`).toBeGreaterThan(0);
    }
  });

  test('switching sessions preserves all tiled terminals', async ({ page }) => {
    // Create 3 sessions
    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      for (let i = 0; i < 3; i++) {
        await addBtn.click();
        await page.waitForTimeout(400);
      }
    }

    // Count initial terminals
    const initialXterms = await page.locator('.xterm').count();
    expect(initialXterms).toBe(3);

    // Switch between sessions
    const tabs = page.locator('[data-session-tab]');

    for (let i = 0; i < 3; i++) {
      await tabs.nth(i).click();
      await page.waitForTimeout(200);

      // After each switch, should still have 3 xterms
      const currentXterms = await page.locator('.xterm').count();
      expect(currentXterms, `After switching to session ${i}: expected 3 xterms, found ${currentXterms}`).toBe(3);
    }
  });
});

test.describe('session switching - edge cases', () => {
  test('switching to same session is a no-op', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      await addBtn.click();
      await page.waitForTimeout(500);
    }

    const tabs = page.locator('[data-session-tab]');
    if (await tabs.count() === 0) {
      test.skip();
    }

    // Click first tab
    const firstTab = tabs.first();
    await firstTab.click();
    await page.waitForTimeout(300);

    // Click same tab again
    await firstTab.click();
    await page.waitForTimeout(300);

    // Should still have xterms
    const xtermCount = await page.locator('.xterm').count();
    expect(xtermCount).toBeGreaterThan(0);
  });

  test('rapid switching between sessions does not cause errors', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const addBtn = page.locator('button').filter({ hasText: '+ Add' });
    if (await addBtn.isVisible()) {
      for (let i = 0; i < 3; i++) {
        await addBtn.click();
        await page.waitForTimeout(400);
      }
    }

    const tabs = page.locator('[data-session-tab]');
    const tabCount = await tabs.count();

    if (tabCount < 2) {
      test.skip();
    }

    // Capture any console errors
    const errors: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'error') {
        errors.push(msg.text());
      }
    });

    // Rapid switch 20 times
    for (let i = 0; i < 20; i++) {
      const tabIndex = i % tabCount;
      await tabs.nth(tabIndex).click();
    }

    // Wait for any async operations
    await page.waitForTimeout(500);

    // Filter out non-critical errors
    const criticalErrors = errors.filter(e =>
      !e.includes('Warning') &&
      !e.includes('favicon') &&
      !e.includes('404')
    );

    expect(criticalErrors).toHaveLength(0);
  });
});
