import { expect, test, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const fixtures = {
  ...defaultFixtures,
  get_mesh_health: null,
  list_agent_nodes: defaultFixtures.list_agent_nodes.map(node => ({
    ...node,
    is_pinned: node.id === 1,
    name: node.id === 1 ? 'fix-terminal-blank-with-a-long-node-name' : node.name,
    status: node.id === 3 ? 'completed' : node.status,
  })),
};

async function openAllNodes(page: Page, groups: number[][] = []) {
  await page.setViewportSize({ width: 900, height: 650 });
  await page.addInitScript(buildInitScript(fixtures, {
    toggle_node_pinned: ({ nodeId }, responses) => {
      const node = responses.list_agent_nodes.find(node => node.id === nodeId);
      node.is_pinned = !node.is_pinned;
      return node;
    },
  }));
  await page.addInitScript(groups => {
    localStorage.setItem('buildmesh.view-mode', 'all');
    localStorage.setItem('buildmesh.node-groups', JSON.stringify(groups));
  }, groups);
  await page.goto('/');
  await expect(page.getByTestId('view-selector')).toHaveAccessibleName(/^All Nodes/);
  await expect(page.locator('[data-session-id="1"]')).toBeVisible();
}

test('All Nodes marks pinned rows and cards and tracks Pin and Unpin in place', async ({ page }) => {
  await openAllNodes(page);
  const cards = page.locator('[data-node-card-id]');
  await expect(cards).toHaveCount(3);
  const order = await cards.evaluateAll(elements => elements.map(element => element.getAttribute('data-node-card-id')));
  const row = page.locator('[data-session-id="1"]');
  const header = page.getByTestId('grid-node-header').and(page.locator('[data-node-id="1"]'));
  const rowPin = row.getByRole('img', { name: 'Pinned', exact: true });
  const headerPin = header.getByRole('img', { name: 'Pinned', exact: true });
  await expect(rowPin).toBeVisible();
  await expect(headerPin).toBeVisible();
  await expect(page.getByTestId('grid-node-header').getByRole('img', { name: 'Pinned', exact: true })).toHaveCount(1);

  await row.click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Unpin node', exact: true }).click();
  await expect(rowPin).toHaveCount(0);
  await expect(headerPin).toHaveCount(0);
  await expect(page.getByTestId('view-selector')).toHaveAccessibleName(/^All Nodes/);
  await header.getByRole('button', { name: 'Agent node actions' }).click();
  await page.getByRole('menuitem', { name: 'Pin node', exact: true }).click();
  await expect(rowPin).toBeVisible();
  await expect(headerPin).toBeVisible();
  expect(await cards.evaluateAll(elements => elements.map(element => element.getAttribute('data-node-card-id')))).toEqual(order);
});

test('a pinned group member is identified on its own tab after selection changes', async ({ page }) => {
  await openAllNodes(page, [[2, 1]]);
  const card = page.locator('[data-node-card-id="2"]');
  const member = card.getByRole('tab', { name: /fix-terminal-blank.*Pinned$/ });
  await expect(member.getByTitle('Pinned', { exact: true })).toBeVisible();
  await member.click();
  await expect(member).toHaveAttribute('aria-selected', 'true');
  await expect(card.getByTestId('grid-node-header')).toHaveAttribute('data-node-id', '1');
  await expect(card.getByTestId('grid-node-header').getByRole('img', { name: 'Pinned', exact: true })).toHaveCount(0);
  await expect(card.getByRole('tab', { name: /diff-panel-refactor/ }).getByTitle('Pinned', { exact: true })).toHaveCount(0);
});
