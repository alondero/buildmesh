import { expect } from '@playwright/test';
import { mkdirSync } from 'node:fs';

// Real dev backend and WebView2 with a completed fixture created through the
// test bridge's production persistence and attachment seams: no agent spawns.
export default async function ({ page, invoke }) {
  const fixture = await invoke('create_test_review_fixture', { name: 'Review consistency verification' });
  const { mesh, source, reviewer } = fixture;
  const output = 'docs/pr-screenshots/hopeful-lifeless-epic';
  mkdirSync(output, { recursive: true });
  try {
    for (const scope of ['node', 'autopilot']) {
      await page.reload({ waitUntil: 'domcontentloaded' });
      const meshItem = page.locator(`#mesh-item-name-${mesh.id}`);
      await meshItem.waitFor({ state: 'visible', timeout: 10000 });
      await meshItem.click();
      const implementation = page.locator(`#activity-${source}-agent-${source}`);
      const review = page.locator(`#activity-${source}-agent-${reviewer}`);
      await expect(implementation).toBeVisible();
      await expect(review).toBeVisible();
      await expect(implementation).toHaveAttribute('aria-selected', 'true');
      await review.click();
      await expect(review).toHaveAttribute('aria-selected', 'true');
      await implementation.click();
      await expect(implementation).toHaveAttribute('aria-selected', 'true');
      const implementationPanel = page.locator(`#activity-panel-${source}`);
      await expect(implementationPanel).toBeVisible();
      const card = await page.locator(`#activity-panel-${source}`).locator('..').boundingBox();
      if (!card) throw new Error('Review node card is not rendered');
      await page.screenshot({ path: `${output}/${scope}-review-activities.png`,
        clip: { ...card, height: Math.min(card.height, 110) } });
    }
    const launch = page.locator(`#activity-panel-${source}`).locator('..').getByRole('button', { name: 'Start review or circuit', exact: true });
    await launch.click();
    await expect(page.getByLabel('Workflow')).toHaveValue('');
    const rounds = page.getByLabel('Maximum review rounds');
    await expect(rounds).toHaveValue('3');
    await rounds.fill('0');
    await expect(page.getByRole('button', { name: 'Start review', exact: true })).toBeDisabled();
    await rounds.fill('3');
    await expect(page.getByRole('button', { name: 'Start review', exact: true })).toBeEnabled();
    await page.getByRole('dialog').screenshot({ path: `${output}/review-dialog-after.png` });
    await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    await expect(page.getByRole('dialog')).toHaveCount(0);
  } finally {
    await invoke('delete_test_review_fixture', { meshId: mesh.id });
    await page.reload({ waitUntil: 'domcontentloaded' });
  }
}
