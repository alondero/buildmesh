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
import { test, expect } from '@playwright/test';
import { waitForTauriReady, invokeViaHttp } from './utils/tauri-http';
import { waitForAppBoot } from './utils/state-waits';

test.describe('sidebar provider dropdown', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/');

    const tauriReady = await waitForTauriReady(8000);
    if (!tauriReady) {
      test.skip();
      return;
    }

    await waitForAppBoot(page);
  });

  test('clicking + button shows provider dropdown', async ({ page }) => {
    await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: `Provider Dropdown Test ${Date.now()}` }
    );

    const plusButton = page.locator('[title="New session"]').first();
    await expect(plusButton, '+ button should render once the project row arrives').toBeVisible({ timeout: 10000 });
    await plusButton.click();

    // Dropdown shows dynamic harness profiles. Terminal is the always-present
    // default; the retired legacy enum rows ("Minimax"/"Kimi") and the "Legacy"
    // header must NOT appear (issue #538).
    await expect(page.locator('text=Terminal').first()).toBeVisible({ timeout: 5000 });
    await expect(page.locator('text=Minimax')).toHaveCount(0);
    await expect(page.locator('text=Kimi')).toHaveCount(0);
    await expect(page.locator('text=Legacy')).toHaveCount(0);
  });

  test('selecting provider creates session and spawns agent', async ({ page }) => {
    await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: `Spawn Test ${Date.now()}` }
    );

    const plusButton = page.locator('[title="New session"]').first();
    await expect(plusButton, '+ button should render once the project row arrives').toBeVisible({ timeout: 10000 });
    await plusButton.click();

    await page.locator('text=Terminal').first().click();

    const sessionItems = page.locator('[data-session-item]');
    await expect(sessionItems, 'a new session row should appear after clicking Terminal').toHaveCount(1, { timeout: 10000 });

    await sessionItems.first().click();
    await expect(page.locator('.xterm'), 'terminal should mount for the active session').toHaveCount(1, { timeout: 10000 });
  });

  test('session has cross button to archive it', async ({ page }) => {
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

    const sessionItem = page.locator('[data-session-item]').first();
    await expect(sessionItem, 'session row should appear after WS push').toBeVisible({ timeout: 10000 });

    const archiveButton = sessionItem.locator('[title="Archive session"]');
    await expect(archiveButton).toBeVisible({ timeout: 5000 });
    await archiveButton.click();

    await expect(
      page.locator('[data-session-item]'),
      'session row should disappear after Archive click',
    ).toHaveCount(0, { timeout: 5000 });
  });
});
