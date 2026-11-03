import { test, expect, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

test.use({ reducedMotion: 'reduce' });

const fixtures = {
  ...defaultFixtures,
  'plugin:app|version': '1.7.0',
};

// The command field is the bar's primary affordance and the app's own
// contract says it stays viewport-centred: the header grid is
// `minmax(0,1fr) auto minmax(0,1fr)` from 1200px up, so both side tracks take
// half of what the field leaves and the field lands on the window's centre
// line. Below 1200px the field yields to the side clusters, so the centring
// assertion starts where the centred grid starts.
const CENTRED_GRID_MIN_WIDTH = 1200;

// The widths the owner document sweeps: the centred-grid boundary and the
// two neighbouring widths, the tier boundaries and their two neighbouring
// widths, plus a wide screen. #2683 put the grid search in the left cluster
// for every View Mode, so the cluster is wider than it was and the tiers
// moved with it (see `titleBarTiers.ts`).
const widths = [1200, 1280, 1539, 1540, 1699, 1700, 1899, 1900, 1920];

// Below the centred grid the tracks are content-sized, so the field sits
// right of centre by design. The band still has to hold together: no cell
// may overflow, and the field may not be squeezed out of the bar.
const yieldsWidths = [900, 1099, 1199];

async function headerGeometry(page: Page) {
  return page.evaluate(() => {
    const header = document.querySelector('header');
    if (!header) throw new Error('the title bar did not render');
    const cells = Array.from(header.children).map(cell => {
      const rect = cell.getBoundingClientRect();
      return {
        left: rect.left,
        right: rect.right,
        width: rect.width,
        scrollWidth: cell.scrollWidth,
        clientWidth: cell.clientWidth,
      };
    });
    const field = document.querySelector('[data-testid="titlebar-command-search"]');
    if (!field) throw new Error('the command field did not render');
    const fieldRect = field.getBoundingClientRect();
    return {
      cells,
      field: {
        left: fieldRect.left,
        right: fieldRect.right,
        width: fieldRect.width,
        centre: fieldRect.left + fieldRect.width / 2,
      },
      viewportCentre: window.innerWidth / 2,
    };
  });
}

for (const width of widths) {
  test(`the command field stays viewport-centred at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.addInitScript(buildInitScript(fixtures));
    await page.goto('/');

    const field = page.getByTestId('titlebar-command-search');
    await expect(field).toBeVisible();
    // The grid search mounts in every mode except Single (issue #2683).
    await expect(page.getByTestId('grid-controls')).toBeVisible();

    const geometry = await headerGeometry(page);
    const displacement = Math.abs(geometry.field.centre - geometry.viewportCentre);
    expect(
      displacement,
      `the command field sits ${displacement.toFixed(1)}px off the window centre at ${width}px`,
    ).toBeLessThanOrEqual(2);

    // No header cell may overflow: the side tracks are capped, so their
    // content has to shrink or truncate rather than run under the field.
    for (const [index, cell] of geometry.cells.entries()) {
      expect(
        cell.scrollWidth,
        `header cell ${index} overflows at ${width}px (${cell.scrollWidth} > ${cell.clientWidth})`,
      ).toBeLessThanOrEqual(cell.clientWidth + 1);
    }

    // The three cells must tile the bar without overlapping: left, centre
    // (the field), right.
    expect(geometry.cells.length).toBeGreaterThanOrEqual(3);
    const [left, centre, right] = geometry.cells;
    expect(left.right).toBeLessThanOrEqual(centre.left + 1);
    expect(centre.right).toBeLessThanOrEqual(right.left + 1);
    expect(geometry.field.left).toBeGreaterThanOrEqual(left.left - 1);
    expect(geometry.field.right).toBeLessThanOrEqual(right.right + 1);
  });
}

for (const width of yieldsWidths) {
  test(`the bar still holds together below ${CENTRED_GRID_MIN_WIDTH}px at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.addInitScript(buildInitScript(fixtures));
    await page.goto('/');

    const field = page.getByTestId('titlebar-command-search');
    await expect(field).toBeVisible();

    const geometry = await headerGeometry(page);
    // Below the breakpoint the side tracks are content-sized, so nothing
    // overflows — but the field may not be squeezed out of the bar either.
    for (const [index, cell] of geometry.cells.entries()) {
      expect(
        cell.scrollWidth,
        `header cell ${index} overflows at ${width}px (${cell.scrollWidth} > ${cell.clientWidth})`,
      ).toBeLessThanOrEqual(cell.clientWidth + 1);
    }
    const [left, centre, right] = geometry.cells;
    expect(left.right).toBeLessThanOrEqual(centre.left + 1);
    expect(centre.right).toBeLessThanOrEqual(right.left + 1);
    expect(geometry.field.width).toBeGreaterThan(0);
  });
}
