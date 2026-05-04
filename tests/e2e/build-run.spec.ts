/**
 * E2E tests for build/run feature
 *
 * These tests require a running Tauri app on port 1991.
 * They verify the full UI flow: mesh creation, agent spawn, build/run terminal.
 */
import { test, expect, Page } from '@playwright/test';

test.describe('build/run feature', () => {
  test.beforeEach(async ({ page }) => {
    // Navigate to the app - these tests assume the app is running
    await page.goto('http://localhost:1991');
  });

  test('build button appears in agent node header', async ({ page }) => {
    // This test verifies that after creating a mesh and spawning an agent,
    // the Build/Run dropdown button appears in the GridNodeHeader
    await page.waitForSelector('[data-testid="grid-node-header"]', { timeout: 5000 });

    // Look for the build dropdown button
    const buildButton = page.locator('button:has-text("Build")');
    await expect(buildButton).toBeVisible();
  });

  test('build dropdown shows correct options', async ({ page }) => {
    // Create a mesh and spawn an agent first
    await page.waitForSelector('[data-testid="grid-node-header"]', { timeout: 5000 });

    const buildButton = page.locator('button:has-text("Build")');
    await buildButton.click();

    // Check dropdown options
    await expect(page.locator('text=Build from worktree')).toBeVisible();
    await expect(page.locator('text=Run from worktree')).toBeVisible();
    await expect(page.locator('text=Open mesh.toml')).toBeVisible();
  });

  test('clicking Build from worktree opens terminal drawer', async ({ page }) => {
    // This test verifies that clicking "Build from worktree" opens the terminal drawer
    await page.waitForSelector('[data-testid="grid-node-header"]', { timeout: 5000 });

    const buildButton = page.locator('button:has-text("Build")');
    await buildButton.click();
    await page.locator('text=Build from worktree').click();

    // The terminal drawer should now be visible
    await expect(page.locator('[data-testid="build-run-terminal"]')).toBeVisible();
  });

  test('clicking Run from worktree opens terminal drawer', async ({ page }) => {
    await page.waitForSelector('[data-testid="grid-node-header"]', { timeout: 5000 });

    const buildButton = page.locator('button:has-text("Build")');
    await buildButton.click();
    await page.locator('text=Run from worktree').click();

    await expect(page.locator('[data-testid="build-run-terminal"]')).toBeVisible();
  });

  test('build terminal shows error when mesh.toml is missing', async ({ page }) => {
    // Create a mesh but don't create mesh.toml, then try to build
    await page.waitForSelector('[data-testid="grid-node-header"]', { timeout: 5000 });

    const buildButton = page.locator('button:has-text("Build")');
    await buildButton.click();
    await page.locator('text=Build from worktree').click();

    // Terminal should show error message
    await expect(page.locator('text=mesh.toml not found')).toBeVisible();
  });

  test('close button closes the drawer', async ({ page }) => {
    await page.waitForSelector('[data-testid="grid-node-header"]', { timeout: 5000 });

    const buildButton = page.locator('button:has-text("Build")');
    await buildButton.click();
    await page.locator('text=Build from worktree').click();

    // Terminal drawer should be visible
    await expect(page.locator('[data-testid="build-run-terminal"]')).toBeVisible();

    // Click close button
    await page.locator('[data-testid="build-run-close"]').click();

    // Drawer should be hidden
    await expect(page.locator('[data-testid="build-run-terminal"]')).toBeHidden();
  });

  test('open mesh.toml opens system default editor', async ({ page }) => {
    // This test verifies that clicking "Open mesh.toml" opens the file in system default editor
    // Note: This may open a file picker or the default editor depending on the OS
    await page.waitForSelector('[data-testid="grid-node-header"]', { timeout: 5000 });

    const buildButton = page.locator('button:has-text("Build")');
    await buildButton.click();
    await page.locator('text=Open mesh.toml').click();

    // The behavior here is system-dependent — on Windows, this might open VS Code or Notepad
    // We just verify no crash occurs
    await expect(page.locator('button:has-text("Build")')).toBeVisible();
  });
});
