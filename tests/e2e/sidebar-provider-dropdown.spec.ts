/**
 * E2E Sidebar Provider Dropdown Tests
 *
 * Tests the new UX flow:
 * - Clicking + button shows a dropdown with agent provider options
 * - Selecting a provider auto-creates a session AND spawns the agent
 * - Each session has a × button to archive/remove it
 *
 * Run with: npx playwright test tests/e2e/sidebar-provider-dropdown.spec.ts
 */
import { test, expect, Page } from '@playwright/test';
import { waitForTauriReady, invokeViaHttp } from './utils/tauri-http';

test.describe('sidebar provider dropdown', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(1000);

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
    }
  });

  test('clicking + button shows provider dropdown', async ({ page }) => {
    // Create a project first so we have something to click +
    const project = await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: `Provider Dropdown Test ${Date.now()}` }
    );
    await page.waitForTimeout(500);

    // Find the + button - it's the button inside the project row with title "New session"
    const plusButton = page.locator('[title="New session"]').first();
    await plusButton.click();
    await page.waitForTimeout(300);

    // Dropdown should appear with provider options
    await expect(page.locator('text=Anthropic').first()).toBeVisible();
    await expect(page.locator('text=Minimax').first()).toBeVisible();
    await expect(page.locator('text=Kimi').first()).toBeVisible();
    await expect(page.locator('text=Agy').first()).toBeVisible();
    await expect(page.locator('text=OpenCode').first()).toBeVisible();
  });

  test('selecting provider creates session and spawns agent', async ({ page }) => {
    const project = await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: `Spawn Test ${Date.now()}` }
    );
    await page.waitForTimeout(500);

    // Click + to open dropdown
    const plusButton = page.locator('[title="New session"]').first();
    await plusButton.click();
    await page.waitForTimeout(300);

    // Click Anthropic to spawn agent
    await page.locator('text=Anthropic').first().click();
    await page.waitForTimeout(1500);

    // A session should now appear in the sidebar
    const sessionItems = page.locator('[data-session-item]');
    await expect(sessionItems).toHaveCount(1, { timeout: 5000 });

    // The session should be active (click it to verify)
    const firstSession = sessionItems.first();
    await firstSession.click();
    await page.waitForTimeout(500);

    // A terminal should be visible
    const terminals = page.locator('.xterm');
    await expect(terminals).toHaveCount(1, { timeout: 5000 });
  });

  test('session has cross button to archive it', async ({ page }) => {
    // Create project and session
    const project = await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: `Archive Test ${Date.now()}` }
    );

    await invokeViaHttp('create_agent_node', {
      meshId: project.id,
      name: 'To Be Archived',
      path: project.path,
      branch: 'main',
    });
    await page.waitForTimeout(500);

    // Find the session item - it should have a × (cross) button
    const sessionItem = page.locator('[data-session-item]').first();

    // The session item should have a × button (title="Archive session")
    const archiveButton = sessionItem.locator('[title="Archive session"]');
    await expect(archiveButton).toBeVisible({ timeout: 3000 });

    // Click the archive button
    await archiveButton.click();
    await page.waitForTimeout(500);

    // Session should be gone from sidebar
    const sessionItems = page.locator('[data-session-item]');
    const count = await sessionItems.count();
    expect(count, 'Session should be archived and removed from sidebar').toBe(0);
  });
});
