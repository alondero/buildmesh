import { expect } from '@playwright/test';

export default async function ({ page }) {
  await page.locator('#mesh-item-name-1').click();
  await page.getByRole('button', { name: 'Search or open' }).click();
  await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
  await page.getByRole('option', { name: /Open Circuits/ }).click();
  await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
  await page.keyboard.press('End');
  await page.locator('[data-testid="circuits-probe-tab"]').waitFor({ state: 'visible' });
  await page.locator('[data-testid="circuit-row"]').first().waitFor({ state: 'visible' });
  await expect(page.getByTestId('circuits-view-activity')).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByTestId('circuit-name-input')).toHaveCount(0);
  await expect(page.getByTestId('queue-run-1002')).toHaveCount(0);
  await expect(page.getByTestId('run-toggle-1001')).toHaveAttribute('aria-expanded', 'true');
  await expect(page.getByTestId('run-error-1001')).toBeVisible();
  await expect(page.getByTestId('run-step-1001-reviewer').locator('pre')).toBeVisible();
  for (const view of ['activity', 'history', 'manage', 'queue']) {
    await page.getByTestId(`circuits-view-${view}`).click();
    if (view === 'manage') {
      await page.getByTestId('circuit-blueprint-select').selectOption('issue_driven_autopilot_review');
      await expect(page.getByTestId('circuit-trigger-label-input')).toBeVisible();
    }
    const clipped = await page.getByTestId('circuits-probe-tab').evaluate((root) => {
      const bounds = root.getBoundingClientRect();
      if (bounds.width > 240) throw new Error('Probe is not at its minimum width');
      return [...root.querySelectorAll('button, select, input')].some((control) => {
        const rect = control.getBoundingClientRect();
        return rect.width > 0 && (rect.left < bounds.left || rect.right > bounds.right);
      });
    });
    expect(clipped).toBe(false);
  }
  await page.getByTestId('circuits-view-queue').click();
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
