import { test, expect, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const fixtures = {
  ...defaultFixtures,
  list_pending_worktree_removals: [],
  get_mesh_health: {
    base_ref: 'origin/main', local_base_branch: 'main', current_branch: 'main', current_short_sha: '1234567',
    is_detached: false, is_dirty: false, unpushed_ahead: 0, has_upstream: true,
    is_drifted: false, base_branch_holder: null, pool_fill_failure: null,
  },
};

async function emitInput(page: Page, nodeId = 2) {
  await page.evaluate(id => {
    // @ts-expect-error Browser mock is installed by buildInitScript.
    window.__BUILDMESH_MOCK__.emit('circuit-agent-blocked', { node_id: id, issue: 0 });
  }, nodeId);
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript({ content: buildInitScript(fixtures) });
  await page.goto('/');
  await expect(page.getByRole('button', { name: 'Open Notifications' })).toBeVisible();
});

test('Circuit input toast names the agent and opens its input across meshes', async ({ page }) => {
  await page.locator('[data-mesh-card="2"]').getByText('playground', { exact: true }).click();
  await emitInput(page);
  const toast = page.getByRole('status').filter({ hasText: /needs your input/ });
  await expect(toast).toContainText('diff-panel-refactor');
  await expect(toast).not.toContainText('issue #0');
  await expect(toast).not.toContainText('node 2');
  await toast.getByRole('button', { name: /Open agent/ }).click();
  await expect(page.locator('[data-node-card-id="2"] .xterm')).toBeVisible();
  await expect(toast).toHaveCount(0);
});

test('dismissed toasts remain actionable in notification history after reload', async ({ page }) => {
  await emitInput(page);
  await page.getByRole('status').filter({ hasText: /needs your input/ })
    .getByRole('button', { name: 'Dismiss notification' }).click();
  await page.reload();
  const bell = page.getByRole('button', { name: 'Open Notifications' });
  await expect(bell).toHaveAttribute('aria-description', '1 unread; 1 need attention');
  await bell.click();
  const attention = page.getByRole('region', { name: 'Needs attention' });
  const open = attention.getByRole('button', { name: /Open agent diff-panel-refactor/ });
  await open.focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('[data-node-card-id="2"] .xterm')).toBeVisible();
  await expect(bell).toHaveAttribute('aria-description', '0 unread; 1 need attention');
});

test('a missing agent reports an inline error without navigating elsewhere', async ({ page }) => {
  await emitInput(page, 6377);
  const toast = page.getByRole('status').filter({ hasText: /needs your input/ });
  await expect(toast).not.toContainText('6377');
  await toast.getByRole('button', { name: /Open agent/ }).click();
  await expect(toast.getByRole('alert')).toContainText('This agent is no longer available.');
  await expect(toast).toBeVisible();
});
