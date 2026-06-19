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

test.describe('file explorer', () => {
  test.beforeAll(async () => {
    const ready = await waitForTauriReady(30000);
    if (!ready) {
      throw new Error('Tauri backend not ready');
    }
  });

  test('folder icon does NOT crash when mesh has no nodes', async ({ page }) => {
    // Create a mesh via HTTP
    const mesh = await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: 'folder-test-empty' }
    );

    // Navigate to app
    await page.goto('http://localhost:1420');
    await page.waitForSelector('img[alt="Buildmesh"]', { timeout: 15000 });
    await page.waitForTimeout(3000);

    // Find mesh row and hover
    const meshRow = page.locator('text="folder-test-empty"');
    if (await meshRow.isVisible({ timeout: 3000 })) {
      await meshRow.hover();
      await page.waitForTimeout(500);

      const folderBtn = page.locator('button[title="Open file explorer"]');
      if (await folderBtn.isVisible()) {
        await folderBtn.click();
        await page.waitForTimeout(2000);

        // Should still be showing the app
        expect(await page.locator('img[alt="Buildmesh"]').isVisible()).toBe(true);
      }
    }
  });

  test('folder icon does NOT crash when mesh has active agent node', async ({ page }) => {
    // Collect console errors
    const consoleErrors: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'error') {
        consoleErrors.push(msg.text());
      }
    });

    // Create a mesh with agent node via HTTP
    const mesh = await invokeViaHttp<{ id: number; name: string; path: string }>(
      'create_test_mesh',
      { name: 'folder-test-with-agent' }
    );

    // Create an agent node
    await invokeViaHttp('create_agent_node', {
      meshId: mesh.id,
      name: 'Test Agent',
      path: mesh.path,
      branch: 'main',
    });

    // Navigate to app
    await page.goto('http://localhost:1420');
    await page.waitForSelector('img[alt="Buildmesh"]', { timeout: 15000 });
    await page.waitForTimeout(3000);

    // Click on an agent node first (user reported this triggers the bug)
    const agentNode = page.locator('text="Test Agent"');
    if (await agentNode.isVisible({ timeout: 3000 })) {
      await agentNode.click();
      await page.waitForTimeout(1000);
    }

    // Find mesh row and hover
    const meshRow = page.locator('text="folder-test-with-agent"');
    if (await meshRow.isVisible({ timeout: 3000 })) {
      await meshRow.hover();
      await page.waitForTimeout(500);

      // Click the folder icon on the mesh row (NOT on a node)
      const folderBtn = page.locator('button[title="Open file explorer"]');
      if (await folderBtn.isVisible()) {
        await folderBtn.click();
        await page.waitForTimeout(3000);

        // Should still be showing the app
        expect(await page.locator('img[alt="Buildmesh"]').isVisible()).toBe(true);

        // Check for React error #321
        const react321Errors = consoleErrors.filter(e => e.includes('321') || e.includes('word-wrap'));
        console.log('Console errors during folder click:', react321Errors);
        expect(react321Errors, 'React error #321 should NOT fire').toHaveLength(0);
      }
    }
  });

  test.afterAll(async () => {
    // No cleanup needed - test projects are temp
  });
});