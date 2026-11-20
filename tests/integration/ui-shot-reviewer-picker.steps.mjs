import { expect } from '@playwright/test';

/** Picks a reviewer Launch Configuration from the review dialog with the
 *  keyboard and leaves the dialog open so the shot can crop to it. */
export default async function ({ page }) {
  await page.locator('#mesh-item-name-1').click();
  const card = page.locator('[data-node-card-id="1"]');
  await expect(card.locator('.xterm')).toBeVisible();
  const review = card.locator('[aria-label="Start review or circuit"]');
  await review.click();
  const dialog = page.getByRole('dialog', { name: 'Start review or circuit' });
  await expect(dialog.getByText('Reviewer launch configuration')).toBeVisible();

  const harness = dialog.getByRole('menuitem', { name: 'Claude', exact: true });
  await harness.focus();
  await page.keyboard.press('ArrowRight');
  const panel = page.getByTestId('spawn-configurations');
  await expect(panel.getByRole('menuitem', { name: 'Parser review configuration', exact: true })).toBeVisible();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');

  await expect(page.getByRole('menu', { name: 'Claude configurations', exact: true })).toHaveCount(0);
  await expect(dialog.getByTestId('reviewer-provider-selection')).toHaveText('Parser review configuration');
  await expect(dialog.getByRole('menuitem', { name: 'Claude', exact: true })).toBeFocused();
}
