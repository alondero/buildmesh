export default async function ({ page }) {
  await page.locator('#mesh-item-name-1').click();
  await page.getByRole('button', { name: 'Search or open' }).click();
  const search = page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' });
  await search.fill('Open Circuits');
  await search.press('Enter');
  await page.locator('[data-testid="circuits-probe-tab"]').waitFor({ state: 'visible' });
  await page.locator('[data-testid="circuit-row"]').first().waitFor({ state: 'visible' });
}
