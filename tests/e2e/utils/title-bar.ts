/**
 * Title-bar entry points for e2e specs (#2421).
 *
 * Below `UTILITIES_IN_BAR_MIN_WIDTH` (1700px, `src/components/TitleBar/titleBarTiers.ts`)
 * the title bar folds Settings, Usage, Zoom and Mobile into the More panel
 * rather than rendering them as bar buttons. These helpers open each one from
 * wherever the bar currently puts it, chosen from the viewport width so the
 * path is deterministic for a given test viewport.
 */
import { expect, Page } from '@playwright/test';

/** Mirrors `UTILITIES_IN_BAR_MIN_WIDTH`; keep the two in step. */
const UTILITIES_IN_BAR_MIN_WIDTH = 1700;

function utilitiesInBar(page: Page): boolean {
  const width = page.viewportSize()?.width;
  if (width === undefined) throw new Error('title-bar helper needs an explicit viewport');
  return width >= UTILITIES_IN_BAR_MIN_WIDTH;
}

/** Open the Settings modal from the title bar, via More when folded. */
export async function openSettingsFromTitleBar(page: Page): Promise<void> {
  if (utilitiesInBar(page)) {
    await page.getByRole('button', { name: 'Open settings', exact: true }).click();
    return;
  }
  await page.getByRole('button', { name: 'More tools', exact: true }).click();
  await expect(page.getByTestId('titlebar-overflow-panel')).toBeVisible();
  await page.getByTestId('titlebar-overflow-settings').click();
}

/** Open the Usage inspector tab from the title bar, via More when folded. */
export async function openUsageFromTitleBar(page: Page): Promise<void> {
  if (utilitiesInBar(page)) {
    await page.getByTestId('titlebar-usage').click();
    return;
  }
  await page.getByRole('button', { name: 'More tools', exact: true }).click();
  await expect(page.getByTestId('titlebar-overflow-panel')).toBeVisible();
  await page.getByTestId('titlebar-overflow-usage').click();
}
