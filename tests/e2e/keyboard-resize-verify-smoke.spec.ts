/**
 * Keyboard-operated pane resizing, verified in a real browser (issue #2424).
 *
 * The sidebar's handle and the agent grid's column/row handles used to
 * answer the mouse only, so a keyboard-only user could not resize a pane
 * at all. This spec drives the REAL app with fixture IPC and checks the
 * painted result in Chromium:
 *
 *   - the sidebar and grid separators are reachable with the keyboard and
 *     announce their name, their orientation and their range;
 *   - the arrow keys move the separator, and the bound the drag enforces
 *     holds at the ends of the range;
 *   - the change is real GEOMETRY — the pane on screen moves, not just a
 *     number in an attribute;
 *   - the focus indicator is painted when the keyboard arrives;
 *   - the sidebar's width is remembered across a reload, the way a
 *     dragged width is;
 *   - the terminal is RETAINED across the resize (the same xterm element
 *     stays in the document — a resize must not remount or dispose it);
 *   - the pointer drag still works after a keyboard resize.
 */
import { test, expect, type Locator, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const SIDEBAR_STORAGE_KEY = 'buildmesh.sidebar-width';

/** The focus token, per theme (`--color-accent-cyan` in App.css). */
const ACCENT = {
  dark: 'rgb(0, 212, 255)',
  light: 'rgb(8, 145, 178)',
} as const;

/**
 * Four agent nodes in one mesh: the grid lays them out as two rows of
 * two columns, so it owns BOTH a column handle per row and a row handle
 * between the rows.
 */
const NODES = [1, 2, 3, 4].map(id => ({
  id,
  mesh_id: 1,
  name: `agent-${id}`,
  path: '/home/user/buildmesh',
  branch: 'origin/main',
  env: 'windows' as const,
  provider: 'anthropic',
  cli_session_id: null,
  worktree_name: null,
  use_worktree: true,
  source_issue: null,
  source_pr: null,
  head_repo_owner: null,
  head_repo_clone_url: null,
  source_pr_pinned_sha: null,
  is_pinned: false,
  signal_health: null,
  worktree_path: null,
  created_at: '1970-01-01T00:00:00Z',
  status: 'running',
  position: id - 1,
}));

const fixtures = {
  ...defaultFixtures,
  list_agent_nodes: NODES,
  'plugin:app|version': '1.8.0',
  // Fixtured rather than left to the mock's unmocked-command fallback: the
  // sidebar asks each mesh for its health on mount, and an unmocked call
  // is console noise this spec has no reason to produce.
  get_mesh_health: {
    base_ref: 'origin/main', local_base_branch: 'main', current_branch: 'main', current_short_sha: '1234567',
    is_detached: false, is_dirty: false, unpushed_ahead: 0, has_upstream: true,
    is_drifted: false, base_branch_holder: null, pool_fill_failure: null,
  },
};

async function boot(page: Page, theme: 'dark' | 'light' = 'dark') {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.addInitScript(buildInitScript(fixtures));
  await page.addInitScript(value => localStorage.setItem('buildmesh.theme', value), theme);
  await page.goto('/');
}

/**
 * Walk the real focus order with Tab until the target holds focus. The
 * app's title bar and rails put several controls in between, so the
 * count is a bound rather than a fixed number of presses.
 */
async function tabTo(page: Page, target: Locator, presses = 60) {
  for (let index = 0; index < presses; index++) {
    if (await target.evaluate(element => element === document.activeElement)) return;
    await page.keyboard.press('Tab');
  }
  throw new Error(`Tab never reached ${await target.getAttribute('aria-label')}`);
}

test.describe('keyboard-operated pane resizing (issue #2424)', () => {
  test('the sidebar separator resizes with the keyboard, shows a focus ring, and remembers the width', async ({ page }) => {
    await boot(page);

    const separator = page.getByRole('separator', { name: 'Resize sidebar' });
    await expect(separator).toBeVisible();
    await expect(separator).toHaveAttribute('aria-orientation', 'vertical');
    await expect(separator).toHaveAttribute('aria-valuemin', '192');
    await expect(separator).toHaveAttribute('aria-valuemax', '480');

    // The sidebar is the dock on the left; its width is the dock's width.
    const dock = page.locator('[data-workspace-dock]').first();
    const dockWidth = async () => (await dock.boundingBox())!.width;

    // Reach the handle with the keyboard — not with a click — so the
    // focus indicator is the keyboard's own.
    await tabTo(page, separator);
    await expect(separator).toBeFocused();
    await expect(separator).toHaveCSS('outline-style', 'solid');
    await expect(separator).toHaveCSS('outline-width', '2px');
    await expect(separator).toHaveCSS('outline-color', ACCENT.dark);

    // End → the minimum, Home → the maximum. Both are real geometry.
    await page.keyboard.press('End');
    await expect.poll(dockWidth).toBeCloseTo(192, 0);

    await page.keyboard.press('Home');
    await expect.poll(dockWidth).toBeCloseTo(480, 0);

    // The bound holds: growing at the maximum leaves the width as it is.
    await page.keyboard.press('ArrowRight');
    await expect.poll(dockWidth).toBeCloseTo(480, 0);

    // One step down is 8px, the same step the Probe panel uses.
    await page.keyboard.press('ArrowLeft');
    await expect.poll(dockWidth).toBeCloseTo(472, 0);

    // The chosen width is remembered, the way a dragged width is.
    const stored = await page.evaluate(key => localStorage.getItem(key), SIDEBAR_STORAGE_KEY);
    expect(stored).toBe('472');

    await page.reload();
    await expect(page.getByRole('separator', { name: 'Resize sidebar' })).toBeVisible();
    await expect
      .poll(async () => (await page.locator('[data-workspace-dock]').first().boundingBox())!.width)
      .toBeCloseTo(472, 0);
  });

  test('the sidebar still drags with the pointer after a keyboard resize', async ({ page }) => {
    await boot(page);

    const separator = page.getByRole('separator', { name: 'Resize sidebar' });
    await expect(separator).toBeVisible();
    const dock = page.locator('[data-workspace-dock]').first();
    const dockWidth = async () => (await dock.boundingBox())!.width;

    await separator.focus();
    await page.keyboard.press('ArrowRight');
    await expect.poll(dockWidth).toBeCloseTo(264, 0);

    // Drag the handle 40px to the left: the sidebar shrinks by 40px.
    const box = (await separator.boundingBox())!;
    const gripX = box.x + box.width / 2;
    const gripY = box.y + 120;
    await page.mouse.move(gripX, gripY);
    await page.mouse.down();
    await page.mouse.move(gripX - 40, gripY, { steps: 5 });
    await page.mouse.up();

    await expect.poll(dockWidth).toBeCloseTo(224, 0);
  });

  test('the grid column and row separators resize with the keyboard and keep the terminal', async ({ page }) => {
    await boot(page);

    const column = page.getByRole('separator', { name: 'Resize column 1 of 2 in row 1' });
    const row = page.getByRole('separator', { name: 'Resize row 1 of 2' });
    await expect(column).toBeVisible();
    await expect(row).toBeVisible();

    await expect(column).toHaveAttribute('aria-orientation', 'vertical');
    await expect(row).toHaveAttribute('aria-orientation', 'horizontal');
    // The announced range is the minimum pane size the drag enforces.
    expect(Number(await column.getAttribute('aria-valuemin'))).toBeGreaterThanOrEqual(300);
    expect(Number(await row.getAttribute('aria-valuemin'))).toBeGreaterThanOrEqual(300);

    // Tag the terminals so we can prove the resize did not remount them.
    const terminalsBefore = await page.evaluate(() => {
      const terms = Array.from(document.querySelectorAll('.xterm'));
      terms.forEach(term => term.setAttribute('data-retained', 'yes'));
      return terms.length;
    });
    expect(terminalsBefore).toBeGreaterThan(0);

    // The column handle moves RIGHT when the pane in front of it grows.
    const columnBefore = (await column.boundingBox())!.x;
    await column.focus();
    await page.keyboard.press('ArrowRight');
    await expect
      .poll(async () => (await column.boundingBox())!.x)
      .toBeGreaterThan(columnBefore + 6);

    // The row handle moves DOWN when the row in front of it grows.
    const rowBefore = (await row.boundingBox())!.y;
    await row.focus();
    await page.keyboard.press('ArrowDown');
    await expect
      .poll(async () => (await row.boundingBox())!.y)
      .toBeGreaterThan(rowBefore + 6);

    // The terminals are still the same elements: a resize must not
    // dispose or remount them.
    const retained = await page.evaluate(() => {
      const terms = Array.from(document.querySelectorAll('.xterm'));
      return {
        total: terms.length,
        retained: terms.filter(term => term.getAttribute('data-retained') === 'yes').length,
      };
    });
    expect(retained.total).toBe(terminalsBefore);
    expect(retained.retained).toBe(terminalsBefore);
  });

  test('the grid still drags with the pointer after a keyboard resize', async ({ page }) => {
    await boot(page);

    const column = page.getByRole('separator', { name: 'Resize column 1 of 2 in row 1' });
    await expect(column).toBeVisible();

    await column.focus();
    await page.keyboard.press('ArrowRight');
    const afterKeyboard = (await column.boundingBox())!.x;

    const box = (await column.boundingBox())!;
    const gripX = box.x + box.width / 2;
    const gripY = box.y + box.height / 2;
    await page.mouse.move(gripX, gripY);
    await page.mouse.down();
    await page.mouse.move(gripX + 40, gripY, { steps: 5 });
    await page.mouse.up();

    await expect
      .poll(async () => (await column.boundingBox())!.x)
      .toBeGreaterThan(afterKeyboard + 30);
  });

  test('the Probe separator takes one step per key press, so a held arrow key does not repeat a step', async ({ page }) => {
    // The Probe panel shipped with a keyboard-resizable handle before this
    // change; the shared primitive must not lose the exactness its
    // screenshot scripts depend on (they press ArrowLeft N times and read
    // the width back).
    await boot(page);

    await page.getByTestId('titlebar-command-search').click();
    await page.getByTestId('command-omnibar-tool-groups')
      .getByRole('button', { name: 'Notes', exact: true })
      .click();

    const separator = page.getByRole('separator', { name: 'Resize probe panel' });
    await expect(separator).toBeVisible();

    await separator.focus();
    await page.keyboard.press('End');
    await expect(separator).toHaveAttribute('aria-valuenow', '240');

    for (let step = 0; step < 5; step++) {
      await page.keyboard.press('ArrowLeft');
    }
    await expect(separator).toHaveAttribute('aria-valuenow', '280');
  });
});
