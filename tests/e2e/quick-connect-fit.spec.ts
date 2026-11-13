/**
 * Quick Connect menu placement in a real browser (issue #2773).
 *
 * The menu used to be placed at a fixed offset from the drop point, with
 * no collision calculation against the editor. A connection dropped near
 * an edge of the canvas put the search box and the results outside the
 * visible editor, where the user could not reach them.
 *
 * The fixture page mounts the real menu in a canvas of a chosen size, so
 * the browser can measure where the menu actually lands at each corner and
 * at each canvas size — the geometry a user sees, not a class string.
 */
import { test, expect } from '@playwright/test';

const CANVASES = [
  { width: 1200, height: 700 },
  { width: 640, height: 420 },
];

/**
 * Drop points in canvas-relative pixels: the four corners and the middle.
 *
 * The drop point is a canvas-relative *screen* position — panning and
 * zooming the graph move the nodes, not the point where the pointer
 * released — so the menu's placement depends on the drop point and the
 * size of the visible editor, and nothing else. Varying the canvas size
 * and the drop point therefore covers the pan and zoom values too.
 */
const DROP_POINTS = [
  { name: 'top-left', x: 4, y: 4 },
  { name: 'top-right', x: 1196, y: 4 },
  { name: 'bottom-left', x: 4, y: 696 },
  { name: 'bottom-right', x: 1196, y: 696 },
  { name: 'middle', x: 600, y: 350 },
];

async function openFixture(page: import('@playwright/test').Page, canvas: { width: number; height: number }, point: { x: number; y: number }) {
  await page.setViewportSize({ width: 1400, height: 900 });
  await page.goto(`/tests/e2e/fixtures/quick-connect-fit.html?width=${canvas.width}&height=${canvas.height}&x=${point.x}&y=${point.y}`);
  await expect(page.locator('[data-testid="quick-connect-menu"]')).toBeVisible();
}

for (const canvas of CANVASES) {
  for (const point of DROP_POINTS) {
    test(`menu stays inside the ${canvas.width}x${canvas.height} editor at the ${point.name}`, async ({ page }) => {
      // The drop point is expressed for the largest canvas; scale it to
      // the canvas under test so it stays a corner.
      const scaled = {
        x: Math.round(point.x * (canvas.width / 1200)),
        y: Math.round(point.y * (canvas.height / 700)),
      };
      await openFixture(page, canvas, scaled);

      const menu = page.locator('[data-testid="quick-connect-menu"]');
      const menuBox = (await menu.boundingBox())!;
      const canvasBox = (await page.locator('#canvas').boundingBox())!;

      expect(menuBox.x, 'the menu should not start left of the editor').toBeGreaterThanOrEqual(canvasBox.x - 0.5);
      expect(menuBox.y, 'the menu should not start above the editor').toBeGreaterThanOrEqual(canvasBox.y - 0.5);
      expect(menuBox.x + menuBox.width, 'the menu should not run past the right edge')
        .toBeLessThanOrEqual(canvasBox.x + canvasBox.width + 0.5);
      expect(menuBox.y + menuBox.height, 'the menu should not run past the bottom edge')
        .toBeLessThanOrEqual(canvasBox.y + canvasBox.height + 0.5);

      // The search box stays visible: it sits outside the scrolling list.
      await expect(page.locator('[data-testid="quick-connect-input"]')).toBeVisible();
    });
  }
}

test('the last result stays reachable when the editor is short', async ({ page }) => {
  await openFixture(page, { width: 640, height: 300 }, { x: 320, y: 290 });

  const list = page.locator('[data-testid="quick-connect-menu"] ul');
  const menu = page.locator('[data-testid="quick-connect-menu"]');
  const menuBox = (await menu.boundingBox())!;
  const listBox = (await list.boundingBox())!;

  // The list is shorter than its content, and it owns the scrolling.
  expect(listBox.height).toBeLessThan(menuBox.height + 1);
  const scrollable = await list.evaluate(element => element.scrollHeight > element.clientHeight + 1);
  expect(scrollable, 'the result list should scroll inside the menu').toBe(true);

  // Scrolling to the end brings the last result into view.
  await list.evaluate(element => { element.scrollTop = element.scrollHeight; });
  const last = list.locator('li').last();
  const lastBox = (await last.boundingBox())!;
  expect(lastBox.y + lastBox.height).toBeLessThanOrEqual(menuBox.y + menuBox.height + 1);
});
