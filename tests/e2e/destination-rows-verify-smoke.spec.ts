import { test, expect, type Locator, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

test.use({ reducedMotion: 'reduce' });

const fixtures = {
  ...defaultFixtures,
  'plugin:app|version': '1.7.0',
  get_mesh_health: {
    base_ref: 'origin/main', local_base_branch: 'main', current_branch: 'main', current_short_sha: '1234567',
    is_detached: false, is_dirty: false, unpushed_ahead: 0, has_upstream: true,
    is_drifted: false, base_branch_holder: null, pool_fill_failure: null,
  },
};

// Independent destination contract: navigation must not silently drop a tool.
const destinations = [
  ['files', 'Project Files'], ['review', 'Agent Changes'],
  ['properties', 'Project Settings'], ['worktrees', 'Repository'],
  ['issues', 'GitHub Issues'], ['pulls', 'GitHub Pull Requests'],
  ['circuits', 'Circuits'], ['prompts', 'Prompts'], ['sessions', 'Agent History'],
  ['scratchpad', 'Notes'], ['usage', 'Usage'], ['notifications', 'Notifications'],
] as const;

async function assertRows(rows: Locator) {
  // Measure after finite opening animations settle, including ancestor scaling.
  await rows.first().evaluate(async element => {
    const animations: Animation[] = [];
    for (let parent: Element | null = element; parent; parent = parent.parentElement) {
      animations.push(...parent.getAnimations());
    }
    await Promise.all(animations.map(animation => animation.finished));
  });
  const geometry = await rows.evaluateAll(elements => elements.map(element => {
    const rect = element.getBoundingClientRect();
    const label = element.querySelector('span')!;
    return { x: rect.x, y: rect.y, width: rect.width, height: rect.height, fits: label.scrollWidth <= label.clientWidth };
  }));
  for (const [index, row] of geometry.entries()) {
    expect(row.height).toBeGreaterThanOrEqual(24);
    expect(row.width).toBeGreaterThanOrEqual(24);
    expect(row.fits).toBe(true);
    if (index > 0) {
      expect(row.x).toBe(geometry[index - 1].x);
      expect(row.y).toBeGreaterThanOrEqual(geometry[index - 1].y + geometry[index - 1].height);
    }
  }
}

async function tabTo(page: Page, target: Locator) {
  // Real Tab traversal includes the embedded terminal-size controls in More.
  for (let index = 0; index < 25; index++) {
    await page.keyboard.press('Tab');
    if (await target.evaluate(element => element === document.activeElement)) return;
  }
  throw new Error(`Tab did not reach ${await target.getAttribute('data-testid')}`);
}

for (const theme of ['dark', 'light']) {
  for (const scale of [100, 150]) {
    test(`${theme}, ${scale}%: compact rows fit and preserve navigation at 900px`, async ({ page }) => {
      await page.setViewportSize({ width: 900, height: 900 });
      await page.addInitScript(buildInitScript(fixtures));
      await page.addInitScript(({ theme, scale }) => {
        localStorage.setItem('buildmesh.theme', theme);
        localStorage.setItem('buildmesh.text-scale', String(scale));
      }, { theme, scale });
      await page.goto('/');
      const more = page.getByTestId('titlebar-overflow');
      const search = page.getByTestId('titlebar-command-search');
      const panel = page.getByTestId('titlebar-overflow-panel');
      const input = page.getByRole('combobox');
      const groups = page.getByTestId('command-omnibar-tool-groups');

      await more.click();
      await assertRows(panel.locator('button[aria-description], button[data-testid$="settings"], button[data-testid$="remote-access"]'));
      await expect(panel.getByRole('button', { name: 'Mobile remote access', exact: true })).toHaveText('Mobile remote access');
      await tabTo(page, page.getByTestId('titlebar-overflow-remote-access'));
      await page.keyboard.press('Escape');
      await expect(more).toBeFocused();

      await search.click();
      await assertRows(groups.getByRole('button'));
      for (const [tab, name] of [...destinations, destinations[0]]) {
        await expect(input).toHaveAttribute('aria-activedescendant', `command-omnibar-tool-${tab}`);
        await expect(groups.getByRole('button', { name, exact: true })).toHaveText(name);
        await expect(input).toBeFocused();
        await page.keyboard.press('ArrowDown');
      }
      await page.keyboard.press('ArrowUp');
      await expect(input).toHaveAttribute('aria-activedescendant', 'command-omnibar-tool-files');
      await page.keyboard.press('ArrowUp');
      await expect(input).toHaveAttribute('aria-activedescendant', 'command-omnibar-tool-notifications');
      await page.keyboard.press('ArrowRight');
      await page.keyboard.press('ArrowLeft');
      await expect(input).toHaveAttribute('aria-activedescendant', 'command-omnibar-tool-notifications');
      await page.keyboard.press('Escape');
      await expect(search).toBeFocused();

      for (const [index, [tab, name]] of destinations.entries()) {
        // Every omnibar destination works with both virtual keyboard focus and pointer.
        await search.click();
        for (let step = 0; step < index; step++) await page.keyboard.press('ArrowDown');
        await page.keyboard.press('Enter');
        await expect(page.getByTestId('probe-context-title')).toHaveText(name);
        await expect(groups).toBeHidden();
        await search.click();
        await groups.getByRole('button', { name, exact: true }).click();
        await expect(page.getByTestId('probe-context-title')).toHaveText(name);

        // Notifications retains its dedicated bell; all other tools are in More at 900px.
        if (tab === 'notifications') continue;
        await more.click();
        const row = page.getByTestId(`titlebar-overflow-${tab}`);
        await expect(row).toHaveAttribute('aria-current', 'true');
        await expect(row).toHaveClass(/bg-bg-selection/);
        await row.click();
        await expect(page.getByTestId('probe-context-title')).toHaveText(name);
        await expect(more).toBeFocused();
        await more.click();
        await tabTo(page, row);
        await page.keyboard.press('Enter');
        await expect(page.getByTestId('probe-context-title')).toHaveText(name);
        await expect(more).toBeFocused();
      }

      for (const [id, dialogName] of [['settings', 'Settings'], ['remote-access', 'Remote Access']]) {
        for (const method of ['pointer', 'keyboard']) {
          await more.click();
          const row = page.getByTestId(`titlebar-overflow-${id}`);
          if (method === 'keyboard') {
            await tabTo(page, row);
            await page.keyboard.press('Enter');
          } else {
            await row.click();
          }
          await expect(page.getByRole('dialog', { name: dialogName, exact: true })).toBeVisible();
          await page.keyboard.press('Escape');
          await expect(more).toBeFocused();
        }
      }

      // Tab still reaches the omnibar's real row buttons, and focus syncs the highlight.
      await search.click();
      await page.keyboard.press('Shift+Tab');
      await page.keyboard.press('Tab');
      await expect(input).toBeFocused();
      // Forward Tab from the input retains the existing Companion shortcut;
      // tab from the first row instead verifies the remaining native button order.
      await groups.getByRole('button', { name: 'Project Files', exact: true }).focus();
      await page.keyboard.press('Tab');
      const focusedRow = groups.getByRole('button', { name: 'Agent Changes', exact: true });
      await expect(focusedRow).toBeFocused();
      await expect(focusedRow).toHaveCSS('outline-style', 'solid');
      await expect(focusedRow).toHaveCSS('outline-color', theme === 'dark' ? 'rgb(0, 212, 255)' : 'rgb(8, 145, 178)');
      await page.keyboard.press('Enter');
      await expect(page.getByTestId('probe-context-title')).toHaveText('Agent Changes');
    });
  }
}
