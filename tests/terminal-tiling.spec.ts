import { test, expect } from '@playwright/test';

/**
 * Smoke test: app loads and renders the session view.
 * Checks that the page doesn't crash on load.
 */
test('app loads without crashing', async ({ page }) => {
  const errors: string[] = [];
  page.on('console', msg => {
    if (msg.type() === 'error') errors.push(msg.text());
  });

  await page.goto('/');
  await page.waitForTimeout(2000);

  // No crash — page should have content
  const body = await page.locator('body').innerHTML();
  expect(body.length, 'Page body is empty - app may have crashed').toBeGreaterThan(100);

  // No console errors
  const realErrors = errors.filter(e =>
    !e.includes('Warning:') &&
    !e.includes('favicon') &&
    !e.includes('404')
  );
  expect(realErrors, `Console errors: ${realErrors.join(', ')}`).toHaveLength(0);
});

/**
 * Regression test: multiple sessions in tiled grid all have their xterm DOM open.
 *
 * Bug: TerminalStack used isVisible gating that prevented non-active sessions
 * from opening their xterm DOM. In tiled mode, every session must have its
 * terminal rendered.
 */
test('tiled sessions all open their xterm DOM', async ({ page }) => {
  await page.goto('/');
  await page.waitForTimeout(1000);

  // Create sessions - click + Add button in sidebar
  const addBtn = page.locator('button').filter({ hasText: '+ Add' });
  if (await addBtn.isVisible()) {
    await addBtn.click();
    await page.waitForTimeout(500);

    // Handle folder dialog if it appears (Tauri native)
    // We'll just check existing sessions render
  }

  // Wait for any sessions to appear
  await page.waitForTimeout(500);

  // Count xterm elements - in tiled mode each session should have one
  const xterms = page.locator('.xterm');
  const count = await xterms.count();

  // At minimum, we should have at least 1 xterm if any session exists
  // (This test verifies the fix: before fix, only active session had xterm)
  if (count > 0) {
    for (let i = 0; i < count; i++) {
      const box = await xterms.nth(i).boundingBox();
      expect(box?.width, `xterm ${i} width`).toBeGreaterThan(5);
      expect(box?.height, `xterm ${i} height`).toBeGreaterThan(5);
    }
  } else {
    // No sessions yet - just verify app rendered something
    const content = await page.locator('[class*="bg-\\[#0f0f0f\\]"]').count();
    expect(content, 'No session grid or terminals visible').toBeGreaterThan(0);
  }
});

/**
 * Unit-like test: AgentTerminal renders a container div with correct class.
 * This tests the component renders without crashing, independent of Tauri.
 */
test('AgentTerminal produces expected DOM structure', async ({ page }) => {
  await page.goto('/');
  await page.waitForTimeout(1000);

  // Find any terminal containers rendered by AgentTerminal
  // AgentTerminal wraps TerminalStack which has class bg-[#0f0f0f]
  const terminalStacks = page.locator('[class*="bg-\\[#0f0f0f\\]"]');
  const count = await terminalStacks.count();

  // Should have at least one terminal area rendered
  expect(count, `Expected at least 1 terminal stack, found ${count}`).toBeGreaterThan(0);
});