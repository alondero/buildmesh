/**
 * Navigation continuity in the browser (issue #2793) — a reload restores the
 * Mesh, the Agent Node and the View Mode the user left behind.
 *
 * The mock-IPC harness serves the shipped fixtures (two Meshes, three Agent
 * Nodes), which is the same scenario the issue reproduced: select the
 * `playground` Mesh, focus the `scratch` node, reload, and the canvas must
 * come back to that scope instead of an empty Mesh Grid.
 *
 * The native dev-profile relaunch is a separate check (a real WebView with a
 * real backend); this spec covers the browser reload path only.
 */
import { expect, test, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const PLAYGROUND_MESH_ID = 2;
const SCRATCH_NODE_ID = 3;

async function boot(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.addInitScript(buildInitScript(defaultFixtures));
  await page.goto('/');
  await expect(scopeLabel(page)).toBeVisible();
  // The Mesh snapshot has loaded and the sidebar has rendered, so the app is
  // past its boot. A cold dependency cache makes Vite reload the page once
  // while it re-optimises; waiting for the sidebar means that reload lands
  // before the interaction, not inside it.
  await expect(page.locator(`#mesh-item-name-${PLAYGROUND_MESH_ID}`)).toBeVisible();
}

/** The scope the canvas shows, as the title bar names it. */
function scopeLabel(page: Page) {
  return page.getByTestId('view-selector');
}

test.describe('navigation continuity across a reload', () => {
  test('restores the selected Mesh and the active Agent Node', async ({ page }) => {
    await boot(page);

    // The user works in the `playground` Mesh and focuses the `scratch` node.
    await page.locator(`#mesh-item-name-${PLAYGROUND_MESH_ID}`).click();
    await expect(scopeLabel(page)).toHaveAttribute('aria-label', 'Mesh Grid · playground');
    await page.locator(`#node-item-name-${SCRATCH_NODE_ID}`).click();

    await page.reload();
    await expect(scopeLabel(page)).toHaveAttribute('aria-label', 'Mesh Grid · playground');

    // Single solos the focused node, so its name in the scope label is the
    // proof that the node — not just the Mesh — came back.
    await scopeLabel(page).click();
    await page.getByTestId('view-selector-mode-single').click();
    await expect(scopeLabel(page)).toHaveAttribute('aria-label', 'Single · scratch · playground');
  });

  test('lands in All Nodes with a notice when the saved Mesh is gone', async ({ page }) => {
    await boot(page);

    // A record whose Mesh no longer exists — deleted between two launches.
    await page.evaluate(() => {
      localStorage.setItem('buildmesh.navigation-snapshot', JSON.stringify({
        version: 1,
        viewMode: 'mesh',
        gridMode: 'mesh',
        selectedMeshId: 99,
        selectedMeshName: 'deleted-mesh',
        activeNodeId: null,
        activeNodeName: null,
      }));
    });

    await page.reload();
    await expect(scopeLabel(page)).toHaveAttribute('aria-label', /^All Nodes · all meshes · \d+ nodes?$/);
    await expect(page.getByText(/deleted-mesh.*no longer exists/)).toBeVisible();
  });

  test('starts from the app defaults when the saved record is corrupt', async ({ page }) => {
    await boot(page);

    await page.evaluate(() => {
      localStorage.setItem('buildmesh.navigation-snapshot', '{not json');
    });

    await page.reload();
    await expect(scopeLabel(page)).toHaveAttribute('aria-label', /^All Nodes · all meshes · \d+ nodes?$/);
    await expect(page.getByText(/no longer exists/)).toHaveCount(0);
  });
});
