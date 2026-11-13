import { expect } from '@playwright/test';

/**
 * Issue #2774 — the compact-control hit-area and hover contract, measured in
 * Chromium against the real frontend with fixture IPC.
 *
 * jsdom applies no CSS, so the unit tests can only pin the class contract.
 * This script proves the rendered result:
 *
 *   - the sidebar's mesh title (selection) and Show/Hide agents (expansion)
 *     controls measure at least 24x24 CSS pixels in BOTH dimensions, and
 *     their adjacent hit areas do not overlap;
 *   - the node header's attention chip measures at least 24x24 CSS pixels
 *     in both dimensions (its height was already 24px — its width was not);
 *   - the title bar's View selector trigger and the grid's removable filter
 *     chips produce a VISIBLE computed background change on hover, in the
 *     dark AND the light theme;
 *   - hovering the View selector does not overwrite the active View Mode's
 *     own colour.
 *
 * The measurement loops below cover the acceptance matrix: 900px and 1440px
 * viewports, 100% and 150% interface text, and both type densities.
 */

const VIEWPORTS = [
  { width: 900, height: 600, label: '900px' },
  { width: 1440, height: 900, label: '1440px' },
];
const TEXT_SCALES = [
  { value: '1', label: '100%' },
  { value: '1.5', label: '150%' },
];
const DENSITIES = ['comfortable', 'compact'];
const THEMES = ['dark', 'light'];
const MIN_TARGET = 24;

export default async function ({ page }) {
  const applyPresentation = async (scale, density, theme) => {
    await page.evaluate(({ scale, density, theme }) => {
      const root = document.documentElement;
      root.style.setProperty('--ui-text-scale', scale);
      if (density === 'comfortable') root.removeAttribute('data-type-density');
      else root.setAttribute('data-type-density', density);
      if (theme === 'light') root.setAttribute('data-theme', 'light');
      else root.removeAttribute('data-theme');
    }, { scale, density, theme });
    // The colour transition is 150ms; the layout settles on the next frame.
    await page.waitForTimeout(300);
  };

  const boxOf = async (locator, what) => {
    const box = await locator.boundingBox();
    if (!box) throw new Error(`${what} has no rendered box`);
    return box;
  };

  const backgroundOf = (locator) =>
    locator.evaluate((el) => getComputedStyle(el).backgroundColor);

  const colourOf = (locator) =>
    locator.evaluate((el) => getComputedStyle(el).color);

  // --- The sidebar mesh row -------------------------------------------
  const meshTitle = page.locator('#mesh-item-name-1');
  const expandToggle = page
    .locator('[data-mesh-card="1"]')
    .getByRole('button', { name: /^(Show|Hide) agents for / });

  await expect(meshTitle).toBeVisible();
  await expect(expandToggle).toBeVisible();

  // The mesh name moved into a child span so the flex button could keep a
  // 24px target and still ellipsize. The ellipsis is checked here rather
  // than assumed: a flex container's own `truncate` would clip the text
  // without an ellipsis, and the name is the row's identity.
  const nameStyle = await meshTitle.locator('span').first().evaluate((el) => {
    const style = getComputedStyle(el);
    return { overflow: style.overflow, textOverflow: style.textOverflow, whiteSpace: style.whiteSpace };
  });
  expect(nameStyle.overflow, 'mesh name overflow').toBe('hidden');
  expect(nameStyle.textOverflow, 'mesh name ellipsis').toBe('ellipsis');
  expect(nameStyle.whiteSpace, 'mesh name wrapping').toBe('nowrap');

  // A long name overflows the name span and is clipped with the ellipsis
  // rather than pushing the row's badges out of the column.
  const nameOverflow = await meshTitle.locator('span').first().evaluate((el) => {
    const original = el.textContent;
    el.textContent = 'a-very-long-mesh-name-that-cannot-fit-the-sidebar-column-at-all';
    const overflowed = el.scrollWidth > el.clientWidth;
    el.textContent = original;
    return { overflowed, clientWidth: el.clientWidth };
  });
  expect(nameOverflow.overflowed, 'a long mesh name does not clip').toBe(true);
  expect(nameOverflow.clientWidth, 'the clipped name column collapsed').toBeGreaterThan(0);

  for (const viewport of VIEWPORTS) {
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    for (const scale of TEXT_SCALES) {
      for (const density of DENSITIES) {
        await applyPresentation(scale.value, density, 'dark');
        const where = `${viewport.label} / ${scale.label} text / ${density} density`;

        const titleBox = await boxOf(meshTitle, `mesh title (${where})`);
        const expandBox = await boxOf(expandToggle, `expand toggle (${where})`);
        console.log(
          `[compact-controls] ${where}: mesh title ${titleBox.width.toFixed(2)}x${titleBox.height.toFixed(2)}px, `
          + `expand toggle ${expandBox.width.toFixed(2)}x${expandBox.height.toFixed(2)}px, `
          + `gap ${(expandBox.y - (titleBox.y + titleBox.height)).toFixed(2)}px`,
        );

        expect(titleBox.width, `mesh title width (${where})`).toBeGreaterThanOrEqual(MIN_TARGET);
        expect(titleBox.height, `mesh title height (${where})`).toBeGreaterThanOrEqual(MIN_TARGET);
        expect(expandBox.width, `expand toggle width (${where})`).toBeGreaterThanOrEqual(MIN_TARGET);
        expect(expandBox.height, `expand toggle height (${where})`).toBeGreaterThanOrEqual(MIN_TARGET);

        // The two adjacent hit areas must not overlap: the title's bottom
        // edge ends above the toggle's top edge.
        expect(
          titleBox.y + titleBox.height,
          `mesh title and expand toggle overlap (${where})`,
        ).toBeLessThanOrEqual(expandBox.y + 0.5);
      }
    }
  }

  // --- The node header attention chip ----------------------------------
  // Node 2 in the fixture is `awaiting_input`, so its card carries the
  // attention chip.
  await page.locator('#mesh-item-name-1').click();
  const attentionChip = page.locator('[data-node-card-id="2"] [data-testid="circuit-outcome-chip"]');
  await expect(attentionChip).toBeVisible();

  for (const viewport of VIEWPORTS) {
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    for (const scale of TEXT_SCALES) {
      for (const density of DENSITIES) {
        await applyPresentation(scale.value, density, 'dark');
        const where = `${viewport.label} / ${scale.label} text / ${density} density`;
        const chipBox = await boxOf(attentionChip, `attention chip (${where})`);
        console.log(
          `[compact-controls] ${where}: attention chip ${chipBox.width.toFixed(2)}x${chipBox.height.toFixed(2)}px`,
        );
        expect(chipBox.width, `attention chip width (${where})`).toBeGreaterThanOrEqual(MIN_TARGET);
        expect(chipBox.height, `attention chip height (${where})`).toBeGreaterThanOrEqual(MIN_TARGET);
      }
    }
  }

  // --- The View selector trigger ---------------------------------------
  const viewSelector = page.getByTestId('view-selector');
  await expect(viewSelector).toBeVisible();
  const activeLabel = viewSelector.locator('.text-accent-cyan').first();

  for (const theme of THEMES) {
    await applyPresentation('1', 'comfortable', theme);
    const before = await backgroundOf(viewSelector);
    const labelBefore = await colourOf(activeLabel);
    await viewSelector.hover();
    await page.waitForTimeout(400);
    const after = await backgroundOf(viewSelector);
    const labelAfter = await colourOf(activeLabel);
    expect(after, `View selector background did not change on hover (${theme})`).not.toBe(before);
    // The accent names the active View Mode — a hover must not overwrite it.
    expect(labelAfter, `View selector label colour changed on hover (${theme})`).toBe(labelBefore);
    console.log(
      `[compact-controls] View selector hover (${theme}): background ${before} -> ${after}, `
      + `active label ${labelBefore} -> ${labelAfter}`,
    );
    await page.mouse.move(0, 0);
  }

  // --- The removable filter chips --------------------------------------
  const search = page.getByTestId('grid-search-input');
  await expect(search).toBeVisible();
  await search.fill('alpha');
  await page.getByRole('button', { name: /Filters and sort/ }).click();
  await page.getByLabel('Filter provider').selectOption('anthropic');
  await page.getByLabel('Filter status').selectOption('running');

  const chips = page.locator('[aria-label="Active filters"] button');
  await expect(chips).toHaveCount(3);

  for (const theme of THEMES) {
    await applyPresentation('1', 'comfortable', theme);
    for (const chip of await chips.all()) {
      const name = await chip.getAttribute('aria-label');
      const box = await boxOf(chip, `filter chip ${name} (${theme})`);
      expect(box.width, `filter chip ${name} width (${theme})`).toBeGreaterThanOrEqual(MIN_TARGET);
      expect(box.height, `filter chip ${name} height (${theme})`).toBeGreaterThanOrEqual(MIN_TARGET);

      const before = await backgroundOf(chip);
      await chip.hover();
      await page.waitForTimeout(400);
      const after = await backgroundOf(chip);
      expect(after, `filter chip ${name} did not change on hover (${theme})`).not.toBe(before);
      console.log(
        `[compact-controls] filter chip ${name} (${theme}): ${box.width.toFixed(2)}x${box.height.toFixed(2)}px, `
        + `background ${before} -> ${after}`,
      );
      await page.mouse.move(0, 0);
    }
  }

  // --- The chips remove their own filter and leave the others --------------
  await page.getByRole('button', { name: /^Remove provider filter/ }).click();
  await expect(page.getByRole('button', { name: /^Remove provider filter/ })).toHaveCount(0);
  await expect(page.getByRole('button', { name: /^Remove search filter/ })).toHaveCount(1);
  await expect(page.getByRole('button', { name: /^Remove status filter/ })).toHaveCount(1);

  // A long search value wraps inside the chip instead of pushing it out of
  // the filter panel: the chip's label carries `break-all`, and the chip
  // itself is capped at the panel's width.
  await search.fill('a'.repeat(120));
  const longChip = page.getByRole('button', { name: /^Remove search filter/ });
  await expect(longChip).toBeVisible();
  const panelBox = await boxOf(page.locator('#grid-filter-panel'), 'filter panel');
  const longBox = await boxOf(longChip, 'long search chip');
  expect(
    longBox.x + longBox.width,
    'a long search chip overflows the filter panel',
  ).toBeLessThanOrEqual(panelBox.x + panelBox.width + 0.5);
  expect(longBox.height, 'a long search chip collapsed').toBeGreaterThanOrEqual(MIN_TARGET);
  await search.fill('alpha');

  // Keyboard reaches the same controls: the title selects, the toggle expands.
  await page.keyboard.press('Escape');
  await meshTitle.focus();
  await page.keyboard.press('Enter');
  await expect(expandToggle).toBeVisible();
}
