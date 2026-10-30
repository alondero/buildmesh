/**
 * Interface text size — the settings control, driven through the real UI.
 *
 * Mock-IPC mode (`scripts/ui-shot.mjs --mock`): proves the frontend renders the
 * row and reacts to the slider, and that the type scale multiplies. It does not
 * prove backend behaviour — nothing here touches the Rust side (the setting is
 * localStorage plus a CSS custom property).
 *
 * Run at a viewport of 1700px or wider so the Settings pill is in the title bar
 * rather than folded into the More panel (`titleBarTiers.ts`).
 */

const READOUT_ID = 'interface-text-scale-value';

/** Computed px size of an element, so "the text got bigger" is measured. */
async function fontSize(locator) {
  return locator.evaluate((el) => parseFloat(getComputedStyle(el).fontSize));
}

export default async function ({ page }) {
  await page.getByRole('button', { name: 'Open settings' }).click();
  await page.getByRole('tab', { name: 'General' }).click();

  const slider = page.getByTestId('interface-text-scale-slider');
  await slider.waitFor({ state: 'visible' });

  const readout = page.getByTestId(READOUT_ID);
  const rowLabel = page.getByText('Interface text size', { exact: true }).first();

  // ---- Default: the shipped size -------------------------------------
  if ((await readout.textContent()).trim() !== '100%') {
    throw new Error(`expected the shipped default 100%, got ${await readout.textContent()}`);
  }
  // Tailwind's text-xs is 0.75rem at the 13px root: 9.75px.
  const baseXs = await fontSize(readout);
  if (Math.abs(baseXs - 9.75) > 0.3) {
    throw new Error(`text-xs should render 9.75px at 100%, measured ${baseXs}`);
  }
  // text-base — the row label — is 1rem = 13px.
  const baseLabel = await fontSize(rowLabel);
  if (Math.abs(baseLabel - 13) > 0.3) {
    throw new Error(`text-base should render 13px at 100%, measured ${baseLabel}`);
  }
  await page.screenshot({
    path: 'docs/pr-screenshots/oily-plush-blossom/text-scale-settings-100.png',
  });

  // ---- Largest size, as a keyboard user reaches it -------------------
  await slider.focus();
  await page.keyboard.press('End');
  await page.waitForFunction(() => localStorage.getItem('buildmesh.text-scale') === '150');

  if ((await readout.textContent()).trim() !== '150%') {
    throw new Error(`readout should show 150%, got ${await readout.textContent()}`);
  }
  if ((await slider.getAttribute('aria-valuetext')) !== '150%') {
    throw new Error('the slider should announce its value as a percentage');
  }
  const scaledXs = await fontSize(readout);
  if (Math.abs(scaledXs / baseXs - 1.5) > 0.02) {
    throw new Error(`text-xs should grow 1.5x, ${baseXs} -> ${scaledXs}`);
  }
  const scaledLabel = await fontSize(rowLabel);
  if (Math.abs(scaledLabel / baseLabel - 1.5) > 0.02) {
    throw new Error(`text-base should grow 1.5x, ${baseLabel} -> ${scaledLabel}`);
  }
  await page.screenshot({
    path: 'docs/pr-screenshots/oily-plush-blossom/text-scale-settings-150.png',
  });

  // ---- Reset returns to the shipped size ------------------------------
  await page.getByTestId('interface-text-scale-reset').click();
  await page.waitForFunction(() => localStorage.getItem('buildmesh.text-scale') === null);
  const resetXs = await fontSize(readout);
  if (Math.abs(resetXs - baseXs) > 0.3) {
    throw new Error(`Reset should restore ${baseXs}px, measured ${resetXs}`);
  }
  if (!(await page.getByTestId('interface-text-scale-reset').isDisabled())) {
    throw new Error('Reset should be disabled at the default size');
  }

  // Leave the app at 150% so the --out screenshot shows the enlarged pane.
  await slider.focus();
  await page.keyboard.press('End');
  await page.waitForFunction(() => localStorage.getItem('buildmesh.text-scale') === '150');
}
