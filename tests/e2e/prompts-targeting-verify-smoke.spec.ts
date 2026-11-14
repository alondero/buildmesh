import { expect, test, type Page } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const BASE_REF = `origin/feature/${'unbroken'.repeat(24)}/meaningful-tail`;
const MESH_NAME = 'Project with a long base branch';
const fixtures = {
  ...defaultFixtures,
  list_meshes: defaultFixtures.list_meshes.map(mesh => mesh.id === 1 ? { ...mesh, name: MESH_NAME, base_ref: BASE_REF } : mesh),
  list_agent_nodes: [],
  list_pending_worktree_removals: [],
  'plugin:app|version': '1.8.0',
  get_mesh_health: null,
  get_mesh_scratchpad: '',
  spawn_agent: null,
  test_prompt_launches: [],
  test_prompt_node: { ...defaultFixtures.list_agent_nodes[0], id: 901, status: 'idle', is_pinned: false },
};

async function boot(page: Page, theme: string, createFailures = 0) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.addInitScript(buildInitScript({ ...fixtures, test_prompt_create_failures: createFailures }, {
    create_agent_node: (args, responses) => {
      responses.test_prompt_launches.push(args);
      if (responses.test_prompt_create_failures > 0) {
        responses.test_prompt_create_failures -= 1;
        throw new Error('Fixture creation failed before node creation');
      }
      const node = { ...responses.test_prompt_node, mesh_id: args.meshId, path: args.path, provider: args.provider };
      responses.list_agent_nodes.push(node);
      return node;
    },
  }));
  await page.addInitScript(theme => {
    localStorage.setItem('buildmesh.theme', theme);
    localStorage.setItem('buildmesh.probe-panel-width', '240');
    localStorage.setItem('buildmesh.view-mode', 'pinned');
  }, theme);
  await page.goto('/');
  await page.getByRole('button', { name: 'Search or open', exact: true }).click();
  await page.getByTestId('command-omnibar-tool-prompts').click();
  await expect(page.getByTestId('prompt-library')).toBeVisible();
}

for (const theme of ['dark', 'light']) {
  test(`primary Spawn Mesh picker preserves keyboard focus and dismisses naturally (${theme})`, async ({ page }, testInfo) => {
    await boot(page, theme);
    const spawn = page.getByRole('button', { name: 'Spawn Explore this project', exact: true });
    const group = page.getByRole('group', { name: 'Launch Explore this project', exact: true });
    const menu = page.getByRole('menu', { name: 'Choose a Mesh to spawn in' });
    const first = page.getByRole('menuitem', { name: MESH_NAME, exact: true });
    const other = page.getByRole('menuitem', { name: 'playground', exact: true });
    await expect(page.getByRole('combobox', { name: 'Spawn in Mesh', exact: true })).toHaveValue('');
    await spawn.focus();
    await page.keyboard.press('Enter');
    await expect(first).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(other).toBeFocused();
    await page.screenshot({ path: testInfo.outputPath('mesh-picker.png') });
    await page.keyboard.press('Escape');
    await expect(menu).toHaveCount(0);
    await expect(spawn).toBeFocused();

    await spawn.click();
    await page.getByRole('searchbox', { name: 'Search prompts' }).click();
    await expect(menu).toHaveCount(0);
    await expect(page.getByRole('searchbox', { name: 'Search prompts' })).toBeFocused();

    await spawn.click();
    await page.keyboard.press('Tab');
    await expect(menu).toHaveCount(0);
    await expect(spawn).not.toBeFocused();
    await expect(group.getByTestId('spawn-dropdown-toggle')).not.toBeFocused();

    await spawn.click();
    await other.click();
    await expect(page.getByRole('status').filter({ hasText: 'Started' })).toContainText('playground');
    await expect(spawn).toBeFocused();
    const calls = await page.evaluate(() => {
      const bridge = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string) => Promise<unknown[]> } };
      return bridge.__TAURI_INTERNALS__.invoke('test_prompt_launches');
    });
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ meshId: 2, provider: 'anthropic' });
    await expect(page.getByRole('button', { name: /Pinned.*all meshes/ })).toBeVisible();
  });

  test(`long launch refs wrap and the Mesh header stripe stretches at 240px (${theme})`, async ({ page }, testInfo) => {
    await boot(page, theme);
    await page.locator('[data-mesh-card="1"]').getByText(MESH_NAME, { exact: true }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Prompts', exact: true }).click();
    const panel = page.getByRole('region', { name: 'Probe panel', exact: true });
    const location = page.getByText(`· from ${BASE_REF}`, { exact: true });
    await expect(location).toBeVisible();
    await expect(page.getByRole('combobox', { name: 'Spawn in Mesh', exact: true })).toHaveValue('1');
    const geometry = await location.evaluate(el => {
      const box = el.getBoundingClientRect();
      const panel = el.closest('section')!.getBoundingClientRect();
      const style = getComputedStyle(el);
      return { width: box.width, height: box.height, lineHeight: parseFloat(style.lineHeight),
        left: box.left, right: box.right, panelLeft: panel.left, panelRight: panel.right,
        panelWidth: panel.width, scrollWidth: el.scrollWidth, clientWidth: el.clientWidth,
        whiteSpace: style.whiteSpace, textOverflow: style.textOverflow };
    });
    expect(geometry.panelWidth).toBe(240);
    expect(geometry.height).toBeGreaterThan(geometry.lineHeight);
    expect(geometry.whiteSpace).toBe('normal');
    expect(geometry.textOverflow).not.toBe('ellipsis');
    expect(geometry.scrollWidth).toBeLessThanOrEqual(geometry.clientWidth + 1);
    expect(geometry.left).toBeGreaterThanOrEqual(geometry.panelLeft);
    expect(geometry.right).toBeLessThanOrEqual(geometry.panelRight);
    const stripe = await page.getByTestId('probe-context-mesh-color').boundingBox();
    const subject = await page.getByTestId('probe-context-subject').boundingBox();
    expect(stripe!.width).toBe(3);
    expect(stripe!.height).toBe(subject!.height);
    await panel.screenshot({ path: testInfo.outputPath('wrap-240.png') });
  });

  test(`custom launch supports Mesh picking and Escape inside the dialog (${theme})`, async ({ page }) => {
    await boot(page, theme);
    await page.getByRole('button', { name: 'Explore this project', exact: true }).click();
    await page.getByRole('button', { name: 'Customise for this launch', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByRole('textbox', { name: 'Prompt for this launch' }).fill('Launch-only draft');
    const spawn = dialog.getByRole('button', { name: 'Spawn Explore this project', exact: true });
    await spawn.click();
    await expect(page.getByRole('menuitem', { name: MESH_NAME, exact: true })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(dialog).toBeVisible();
    await expect(spawn).toBeFocused();
    await spawn.click();
    await page.keyboard.press('Tab');
    await expect(page.getByRole('menu', { name: 'Choose a Mesh to spawn in' })).toHaveCount(0);
    await expect(dialog).toBeVisible();
    await expect(spawn).not.toBeFocused();
    await spawn.click();
    await page.keyboard.press('ArrowDown');
    await expect(page.getByRole('menuitem', { name: 'playground', exact: true })).toBeFocused();
    if (theme === 'light') await page.getByRole('menuitem', { name: 'playground', exact: true }).click();
    else await page.keyboard.press('Enter');
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole('status').filter({ hasText: 'Started' })).toContainText('playground');
    const calls = await page.evaluate(() => {
      const bridge = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string) => Promise<unknown[]> } };
      return bridge.__TAURI_INTERNALS__.invoke('test_prompt_launches');
    });
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ meshId: 2, initialPrompt: 'Launch-only draft' });
  });

  test(`custom picker Mesh and draft survive failed creation and retry (${theme})`, async ({ page }, testInfo) => {
    await boot(page, theme, 1);
    await page.getByRole('button', { name: 'Explore this project', exact: true }).click();
    await page.getByRole('button', { name: 'Customise for this launch', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByRole('textbox', { name: 'Prompt for this launch' }).fill('Keep this launch draft');
    const spawn = dialog.getByRole('button', { name: 'Spawn Explore this project', exact: true });
    await spawn.click();
    await page.getByRole('menuitem', { name: 'playground', exact: true }).click();
    await expect(dialog.getByRole('alert')).toContainText('Fixture creation failed before node creation');
    await expect(dialog.getByRole('combobox', { name: 'Spawn in Mesh' })).toHaveValue('2');
    await expect(dialog.getByRole('textbox', { name: 'Prompt for this launch' })).toHaveValue('Keep this launch draft');
    await dialog.screenshot({ path: testInfo.outputPath('failed-create-target.png') });
    await spawn.click();
    await expect(page.getByRole('menu', { name: 'Choose a Mesh to spawn in' })).toHaveCount(0);
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole('status').filter({ hasText: 'Started' })).toContainText('playground');
    const calls = await page.evaluate(() => {
      const bridge = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string) => Promise<unknown[]> } };
      return bridge.__TAURI_INTERNALS__.invoke('test_prompt_launches');
    });
    expect(calls).toHaveLength(2);
    for (const call of calls) expect(call).toMatchObject({ meshId: 2, initialPrompt: 'Keep this launch draft' });
  });
}
