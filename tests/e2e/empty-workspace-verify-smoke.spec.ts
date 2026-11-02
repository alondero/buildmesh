import { test, expect } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';
import { openSettingsFromTitleBar } from './utils/title-bar';

const fixtures = {
  ...defaultFixtures,
  'plugin:app|version': '1.7.0',
  get_mesh_health: {
    base_ref: 'origin/main', local_base_branch: 'main', current_branch: 'main', current_short_sha: '1234567',
    is_detached: false, is_dirty: false, unpushed_ahead: 0, has_upstream: true,
    is_drifted: false, base_branch_holder: null, pool_fill_failure: null,
  },
};

// Real frontend interactions with fixture IPC; no native launch is exercised.
for (const width of [900, 1440]) {
  test(`first repository has one creation action at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 720 });
    const mesh = defaultFixtures.list_meshes[0];
    await page.addInitScript(buildInitScript({
      ...fixtures, list_meshes: [], list_agent_nodes: [],
      pick_mesh_folder: { name: mesh.name, path: mesh.path }, create_mesh: mesh,
    }));
    await page.goto('/');
    const create = page.getByRole('button', { name: 'New mesh', exact: true });
    await expect(create).toHaveCount(1);
    await expect(page.getByRole('separator', { name: 'Resize sidebar' })).toHaveCount(0);
    await create.focus();
    await page.keyboard.press('Enter');
    const dialog = page.getByRole('dialog', { name: 'New mesh' });
    await expect(dialog.getByRole('button', { name: 'Choose folder…' })).toBeFocused();
    await page.keyboard.press('Enter');
    await dialog.getByRole('button', { name: 'Create mesh', exact: true }).click();
    await expect(dialog).toBeHidden();
    await expect(page.getByTestId('canvas-empty-spawn-agent')).toBeVisible();
    await expect(page.getByRole('separator', { name: 'Resize sidebar' })).toBeVisible();
    // The persistent sidebar action appears only after the first mesh exists.
    await expect(create).toHaveCount(1);
    await page.getByTestId('canvas-empty-spawn-agent').click();
    await expect(page.getByRole('dialog', { name: `Spawn agent in ${mesh.name}` })).toBeVisible();
  });

  for (const theme of ['dark', 'light']) {
    test(`settings controls and contextual help work at ${width}px in ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 720 });
      await page.addInitScript(buildInitScript({ ...fixtures, list_meshes: [], list_agent_nodes: [] }));
      await page.addInitScript(value => localStorage.setItem('buildmesh.theme', value), theme);
      await page.goto('/');
      await openSettingsFromTitleBar(page);
      const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
      const themeRadio = dialog.getByRole('radio', { name: theme === 'dark' ? 'light' : 'dark', exact: true });
      await themeRadio.check();
      await expect(themeRadio).toBeChecked();
      await expect(dialog.getByText('Colour theme for the app and terminals.')).toHaveCount(0);
      const help = dialog.getByRole('button', { name: 'About Interface text size' });
      await help.focus();
      await page.keyboard.press('Enter');
      await expect(page.getByRole('tooltip')).toContainText('100%');
      await page.keyboard.press('Escape');
      await expect(page.getByRole('tooltip')).toBeHidden();
      await expect(help).toBeFocused();
      await expect(dialog).toBeVisible();
      const slider = dialog.getByRole('slider', { name: 'Interface text size' });
      await slider.press('End');
      await expect(slider).toHaveAttribute('aria-valuetext', '150%');
      const bounds = await dialog.boundingBox();
      const control = await slider.boundingBox();
      expect(bounds).not.toBeNull();
      expect(control).not.toBeNull();
      expect(control!.x).toBeGreaterThanOrEqual(bounds!.x);
      expect(control!.x + control!.width).toBeLessThanOrEqual(bounds!.x + bounds!.width);
      expect(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    });
  }
}

test('an empty mesh keeps Terminal available while setup is unavailable', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 600 });
  await page.addInitScript(buildInitScript({ ...fixtures, list_agent_nodes: [], list_providers: [] }));
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Set up an agent' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Start Terminal', exact: true })).toBeEnabled();
  await page.getByTestId('canvas-empty-open-setup').click();
  await expect(page.getByRole('tab', { name: 'Providers', exact: true })).toHaveAttribute('aria-selected', 'true');
});
