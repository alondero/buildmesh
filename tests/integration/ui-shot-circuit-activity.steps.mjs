// The Activity summary's own inspection route (issue #2740).
//
// The shared boot fixture reproduces the reported state: no active runs, two
// queued runs and one finished run that still needs a person. This route
// checks that the Activity view states that truthfully, that both routes work
// from the keyboard, and that the summary survives the widths, themes and
// text sizes the Probe dock has to render in.
import { expect } from '@playwright/test';

/** The Probe dock resizes between 240px and 720px; 240px is the design case. */
const MIN_WIDTH = 240;

/** The largest interface text size the app offers (Settings → General). */
const LARGE_TEXT_SCALE = '1.5';

async function setTheme(page, theme) {
  // `theme.ts` drives the palette through `<html data-theme>`; dark is the
  // default and removes the attribute, light sets it.
  await page.evaluate((name) => {
    if (name === 'light') document.documentElement.setAttribute('data-theme', 'light');
    else document.documentElement.removeAttribute('data-theme');
  }, theme);
}

async function setTextScale(page, scale) {
  // The `text-*` steps read `--ui-text-scale`, the one variable `textScale.ts`
  // writes on <html>.
  await page.evaluate((value) => {
    document.documentElement.style.setProperty('--ui-text-scale', value);
  }, scale);
}

async function assertNothingClipped(page, label) {
  const clipped = await page.getByTestId('circuits-probe-tab').evaluate((root) => {
    const bounds = root.getBoundingClientRect();
    const escaped = [...root.querySelectorAll('button, select, input')].filter((control) => {
      const rect = control.getBoundingClientRect();
      return rect.width > 0 && (rect.left < bounds.left - 0.5 || rect.right > bounds.right + 0.5);
    }).map((control) => control.textContent?.trim() || control.getAttribute('aria-label'));
    const body = root.querySelector('[data-testid="circuits-probe-body"]');
    return {
      escaped,
      scroll: body ? body.scrollWidth : 0,
      client: body ? body.clientWidth : 0,
    };
  });
  expect(clipped.escaped, `${label}: a control escaped the Probe`).toEqual([]);
  expect(clipped.scroll, `${label}: the Activity body scrolled sideways`).toBeLessThanOrEqual(clipped.client);
}

export default async function ({ page }) {
  await page.locator('#mesh-item-name-1').click();
  await page.getByRole('button', { name: 'Search or open' }).click();
  await page.getByRole('combobox', { name: 'Search commands, nodes, meshes and more' }).fill('Open Circuits');
  await page.getByRole('option', { name: /^Open Circuits Inspect/ }).click();
  await page.getByTestId('circuits-probe-tab').waitFor({ state: 'visible' });

  // ---- The summary states the three dimensions apart -----------------------
  await expect(page.getByTestId('circuit-activity-summary-label')).toHaveText('1 finished run needs attention');
  await expect(page.getByTestId('circuit-activity-summary-detail')).toHaveText('No active runs · 2 queued');
  // The old copy claimed readiness from the slot count alone.
  await expect(page.getByText('Ready for work')).toHaveCount(0);

  // ---- The attention route leads to the filtered History, from the keyboard --
  await page.getByTestId('circuit-activity-attention-action').focus();
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('circuits-view-history')).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByTestId('history-attention-toggle')).toBeChecked();
  await expect(page.getByTestId('run-card-1001')).toBeVisible();

  // ---- The queued-work context and its View queue action survive ------------
  await page.getByTestId('circuits-view-activity').click();
  await expect(page.getByTestId('circuit-activity-queue-summary')).toContainText('Waiting in queue');
  await expect(page.getByTestId('circuit-activity-queue-summary')).toContainText('2 waiting to start');
  await page.getByRole('button', { name: 'View queue' }).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('circuits-view-queue')).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByTestId('queue-run-1002')).toBeVisible();
  await page.getByTestId('circuits-view-activity').click();

  // ---- Widths, themes and text sizes ---------------------------------------
  for (const theme of ['dark', 'light']) {
    for (const scale of ['1', LARGE_TEXT_SCALE]) {
      await setTheme(page, theme);
      await setTextScale(page, scale);
      const label = `${theme} / ${scale}x`;

      // The default Probe width.
      await expect(page.getByTestId('circuit-activity-summary')).toBeVisible();
      await assertNothingClipped(page, `${label} default width`);

      // The dock's minimum width — the case to design for.
      await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
      await page.keyboard.press('End');
      const width = await page.getByTestId('circuits-probe-tab').evaluate((root) => root.getBoundingClientRect().width);
      expect(width, `${label}: the Probe did not reach its minimum width`).toBeLessThanOrEqual(MIN_WIDTH + 1);
      await expect(page.getByTestId('circuit-activity-summary-label')).toBeVisible();
      await expect(page.getByTestId('circuit-activity-summary-detail')).toBeVisible();
      await expect(page.getByTestId('circuit-activity-attention-action')).toBeVisible();
      await assertNothingClipped(page, `${label} 240px`);

      await page.getByTestId('circuits-probe-tab').screenshot({
        path: `.tmp/circuit-activity-summary-${theme}-${scale}x-240.png`,
      });

      // Back to the default width for the next combination.
      await page.getByRole('separator', { name: 'Resize probe panel' }).focus();
      await page.keyboard.press('Home');
    }
  }

  console.log(
    'PASS: the Activity summary states running, queued and attention work apart in both themes ' +
      'at 1x and 1.5x text, at the default Probe width and at the 240px minimum, with no clipped control.'
  );
}
