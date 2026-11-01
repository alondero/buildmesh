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
  await page.addInitScript(() => localStorage.setItem('buildmesh.probe-panel-width', '240'));
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
  await expect(toast.getByRole('button', { name: /Open agent/ })).toHaveAccessibleName(/needs your input.*Open agent/);
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
  const open = attention.getByRole('button', { name: /diff-panel-refactor.*needs your input.*Open agent/ });
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
  const bell = page.getByRole('button', { name: 'Open Notifications' });
  await bell.click();
  const tab = page.getByTestId('notifications-tab');
  const dismiss = tab.getByRole('region', { name: 'Needs attention' }).getByRole('button', { name: 'Dismiss notification from Circuit agent' });
  const bounds = await dismiss.boundingBox();
  const tabBounds = await tab.boundingBox();
  expect(bounds!.width).toBeGreaterThanOrEqual(24);
  expect(bounds!.height).toBeGreaterThanOrEqual(24);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(tabBounds!.x + tabBounds!.width);
  expect(await tab.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await dismiss.focus();
  await page.keyboard.press('Enter');
  await expect(bell).toHaveAttribute('aria-description', '0 unread; 0 need attention');
  await expect(tab.getByRole('region', { name: 'Recent activity' })).toContainText('Notification dismissed.');
  await expect(toast).toHaveCount(0);
});

test('fallback toast and saved names refresh when the missing agent arrives', async ({ page }) => {
  await emitInput(page, 6377);
  const toast = page.getByRole('status').filter({ hasText: /needs your input/ });
  await expect(toast).toContainText('Circuit agent needs your input');
  const lateNode = { ...fixtures.list_agent_nodes[1], id: 6377, name: 'late-loaded-agent', position: 2 };
  await page.evaluate(({ nodes, id }) => {
    // @ts-expect-error Browser mock is installed by buildInitScript.
    const mock = window.__BUILDMESH_MOCK__;
    mock.on('list_agent_nodes', nodes);
    mock.emit('node-created', { id });
  }, {
    // The initial idle shell has already entered the mock's spawn handler.
    // A later snapshot must retain that state rather than re-arm auto-launch.
    nodes: [...fixtures.list_agent_nodes.map(node => node.status === 'idle' ? { ...node, status: 'spawning' } : node), lateNode],
    id: lateNode.id,
  });
  await expect(toast).toContainText('late-loaded-agent in buildmesh needs your input');
  await expect(toast.getByRole('button', { name: /Open agent/ })).toHaveAccessibleName(/late-loaded-agent in buildmesh needs your input/);
  await page.getByRole('button', { name: 'Open Notifications' }).click();
  await expect(page.getByRole('region', { name: 'Needs attention' })).toContainText('late-loaded-agent in buildmesh needs your input');
});
