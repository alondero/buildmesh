export default async function ({ page }) {
  await page.locator('#mesh-item-name-1').click();
  await page.getByRole('button', { name: 'Circuits' }).click();
  await page.locator('[data-testid="circuits-probe-tab"]').waitFor({ state: 'visible' });
  await page.locator('[data-testid="circuit-row"]').first().waitFor({ state: 'visible' });
}
