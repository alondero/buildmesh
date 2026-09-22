import { expect } from '@playwright/test';

/** Actual pointer geometry and reload persistence, with mock IPC only. */
export default async function ({ page }) {
  const cards = page.locator('[data-node-card-id]');
  const card = id => page.locator(`[data-node-card-id="${id}"]`);
  const drag = async (source, target, portion, release = true) => {
    const header = await card(source).getByTestId('grid-node-header').boundingBox();
    const bounds = await card(target).boundingBox();
    await page.mouse.move(header.x + 5, header.y + header.height / 2);
    await page.mouse.down();
    await page.mouse.move(header.x + 15, header.y + header.height / 2, { steps: 3 });
    await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height * portion, { steps: 15 });
    if (release) await page.mouse.up();
  };
  await expect(cards).toHaveCount(3);
  await drag(1, 2, 0.2, false);
  await expect(page.getByText('Group as tabs', { exact: true })).toBeVisible();
  await expect(page.getByText('⇄ Swap', { exact: true })).toBeVisible();
  await page.mouse.up();
  await expect(cards).toHaveCount(2);
  await expect(card(2).getByRole('tab')).toHaveCount(2);
  await expect(card(2).getByRole('tab').nth(1)).toHaveAttribute('aria-selected', 'true');
  await card(2).getByRole('tab').first().focus();
  await page.keyboard.press('ArrowRight');
  await expect(card(2).getByRole('tab').nth(1)).toBeFocused();
  await page.reload({ waitUntil: 'domcontentloaded' });
  await expect(cards).toHaveCount(2);
  await expect(card(2).getByRole('tab')).toHaveCount(2);

  // Preserve position writes across the frontend's normal polling/refetches.
  await page.evaluate(async () => {
    let nodes = await window.__TAURI_INTERNALS__.invoke('list_agent_nodes');
    window.__BUILDMESH_MOCK__.on('list_agent_nodes', () => nodes);
    window.__BUILDMESH_MOCK__.on('update_agent_node_positions', ({ updates }) => {
      const positions = new Map(updates);
      nodes = nodes.map(node => ({ ...node, position: positions.get(node.id) ?? node.position }))
        .sort((a, b) => a.position - b.position);
    });
  });
  const before = await card(2).boundingBox();
  await drag(2, 3, 0.75);
  await expect.poll(async () => (await card(3).boundingBox()).x).toBe(before.x);
  await expect.poll(async () => (await card(2).boundingBox()).x).toBeGreaterThan(before.x);
  await card(2).getByRole('button', { name: 'Move selected node out of group' }).click();
  await expect(cards).toHaveCount(3);
  await drag(1, 2, 0.75);
  await expect.poll(() => cards.evaluateAll(els => els.map(el => el.getAttribute('data-node-card-id'))))
    .toEqual(['2', '3', '1']);
  await drag(1, 2, 0.2);
  await expect(cards).toHaveCount(2);

  await page.setViewportSize({ width: 730, height: 700 });
  await expect.poll(async () => (await card(2).boundingBox()).width).toBeLessThanOrEqual(240);
  const sessions = card(2).getByRole('button', { name: 'All sessions (2)' });
  await expect(sessions).toBeVisible();
  await sessions.click();
  await expect(page.getByRole('menu', { name: 'All sessions' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(sessions).toBeFocused();
  await page.setViewportSize({ width: 1440, height: 900 });
}
