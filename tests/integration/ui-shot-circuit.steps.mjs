export default async function ({ page }) {
  await page.locator('#mesh-item-name-1').click();
  await page.getByRole('button', { name: 'Search or open' }).click();
  await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
  await page.getByRole('option', { name: /Open Circuits/ }).click();
  await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
  await page.keyboard.press('End');
  await page.locator('[data-testid="circuits-probe-tab"]').waitFor({ state: 'visible' });
  await page.locator('[data-testid="circuit-row"]').first().waitFor({ state: 'visible' });
  await page.locator('[data-testid="queue-run-1002"]').waitFor({ state: 'visible' });
  await page.evaluate(() => {
    const row = document.querySelector('[data-testid="queue-run-1002"]');
    if (!row) throw new Error('queue fixture row did not render at minimum width');
    const rowRect = row.getBoundingClientRect();
    for (const button of row.querySelectorAll('button')) {
      const rect = button.getBoundingClientRect();
      if (rect.left < rowRect.left || rect.right > rowRect.right) {
        throw new Error('queue action button is clipped at the 240px probe width');
      }
    }
  });
}
