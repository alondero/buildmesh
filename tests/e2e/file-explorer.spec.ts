/**
 * E2E test for file explorer bug reproduction
 *
 * Bug: clicking folder icon on a mesh with active agent blanks the window.
 *
 * REPRO STEPS (user reported):
 * 1. Start buildmesh
 * 2. Create a mesh with agent node
 * 3. Click the folder icon on the mesh row in the sidebar
 * 4. Window goes blank
 */
import { test, expect } from '@playwright/test';
import { invokeViaHttp, waitForTauriReady } from './utils/tauri-http';
import { waitForAppBoot } from './utils/state-waits';

test.describe('file explorer', () => {
  test.beforeAll(async () => {
    const ready = await waitForTauriReady(30000);
    if (!ready) {
      throw new Error('Tauri backend not ready');
    }
  });

  test('folder icon does NOT crash when mesh has no nodes', async ({ page }) => {
    await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: 'folder-test-empty' }
    );

    await page.goto('/');
    await waitForAppBoot(page);

    const meshRow = page.locator('text="folder-test-empty"');
    await expect(meshRow).toBeVisible({ timeout: 10000 });
    await meshRow.hover();

    const folderBtn = page.locator('button[title="Open file explorer"]');
    await expect(folderBtn).toBeVisible({ timeout: 5000 });
    await folderBtn.click();

    await expect(page.locator('img[alt="Buildmesh"]'), 'app should remain mounted after the folder click').toBeVisible({ timeout: 10000 });
  });

  test('folder icon does NOT crash when mesh has active agent node', async ({ page }) => {
    const consoleErrors: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'error') {
        consoleErrors.push(msg.text());
      }
    });

    const mesh = await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: 'folder-test-with-agent' }
    );

    await invokeViaHttp('create_agent_node', {
      meshId: mesh.id,
      name: 'Test Agent',
      path: mesh.path,
      branch: 'main',
    });

    await page.goto('/');
    await waitForAppBoot(page);

    const agentNode = page.locator('text="Test Agent"');
    await expect(agentNode).toBeVisible({ timeout: 10000 });
    await agentNode.click();

    const meshRow = page.locator('text="folder-test-with-agent"');
    await expect(meshRow).toBeVisible({ timeout: 10000 });
    await meshRow.hover();

    const folderBtn = page.locator('button[title="Open file explorer"]');
    await expect(folderBtn).toBeVisible({ timeout: 5000 });
    await folderBtn.click();

    await expect(page.locator('img[alt="Buildmesh"]'), 'app should remain mounted after the folder click').toBeVisible({ timeout: 10000 });

    // Brief settle so late console errors (async chunks triggered by
    // the click) have a chance to flush — stream-based, no DOM state.
    await page.waitForTimeout(200);
    const react321Errors = consoleErrors.filter(e => e.includes('321') || e.includes('word-wrap'));
    console.log('Console errors during folder click:', react321Errors);
    expect(react321Errors, 'React error #321 should NOT fire').toHaveLength(0);
  });

  test.afterAll(async () => {
    // No cleanup needed - test projects are temp
  });
});
