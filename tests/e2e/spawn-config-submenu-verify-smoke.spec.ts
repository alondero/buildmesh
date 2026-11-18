/**
 * Spawn-configuration submenu placement — browser regression.
 *
 * The configuration submenu is a side flyout of the harness row it was opened
 * from. Two placement defects were found in review:
 *
 *  1. The editor owns its own draft state: switching to a routed provider
 *     adds endpoint fields without changing the submenu's props, so the
 *     placement had to follow the panel's content size. It did not — the
 *     panel kept the top it was given for the shorter form and grew
 *     downwards across the anchor row and past the viewport.
 *  2. The room above and below the anchor was measured from the raw anchor
 *     rect, so a row that is only partly visible at a scroll boundary could
 *     hand the panel room past the viewport's bottom edge.
 *
 * The spec drives the real frontend in Chromium against the mocked Tauri IPC
 * (scripts/ui-mock/tauri-mock.mjs), so the placement runs with real layout,
 * real popovers and a real ResizeObserver.
 *
 * Geometry: the sidebar is filled with enough mesh cards that the spawn row
 * sits near the bottom of the window, and the harness list is long enough
 * that its last row is pinned to the viewport's bottom edge. The submenu
 * therefore opens above its row, which is the placement that breaks when the
 * editor grows.
 *
 * Run standalone: npx playwright test --project=verify-smoke
 */
import { test, expect } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

const HARNESS_ID = 'codex';

/**
 * Harness rows in menu order. The configured harness is last so its row is
 * the one pinned to the bottom of the harness list.
 */
const harnessIds = ['alpha', 'bravo', 'charlie', 'delta', 'echo', HARNESS_ID];

const capabilities = {
  harness_id: HARNESS_ID,
  background_inference: null,
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: false,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: true,
  supports_extra_args: true, supports_agent: false,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: 'inline_config',
  permission_modes: [],
  default_permission_mode: null,
  available_on: ['windows'],
};

const providerRows = harnessIds.map((harnessId) => ({
  id: harnessId,
  label: harnessId,
  color: '#00d4ff',
  icon: 'codex',
  resumable: true,
  harness_id: harnessId,
  provider_id: null,
  is_proxied: false,
  group_key: harnessId,
  runtime: 'windows',
  capabilities: { ...capabilities, harness_id: harnessId },
}) satisfies ProviderInfo[]);

const savedConfiguration = {
  id: 'launch/sol',
  name: 'Sol Max',
  spawn_option_id: HARNESS_ID,
  harness_id: HARNESS_ID,
  model: null,
  effort: null,
  extra_args: null,
};

/**
 * A routed target for the same harness. Picking it in the editor adds the
 * endpoint fields, which is the content growth the placement has to follow.
 */
const route = {
  harness_id: HARNESS_ID,
  provider_id: 'minimax',
  surface: 'openai' as const,
  base_url: 'https://api.minimax.io/v1',
  model_tiers: { default: 'MiniMax-M3', opus: null, fable: null, sonnet: null, haiku: null, small_fast: null },
};

const fixtures = {
  ...defaultFixtures,
  // Enough mesh cards that the last one can be scrolled to the bottom of the
  // sidebar, which puts its spawn row low in the window.
  list_meshes: Array.from({ length: 8 }, (_, index) => ({
    ...defaultFixtures.list_meshes[0],
    id: index + 1,
    name: `mesh-${index + 1}`,
  })),
  list_agent_nodes: [],
  list_providers: providerRows,
  list_spawn_configurations: [savedConfiguration],
  get_launch_targets: [
    // The native target keeps the first form short: no model, no effort and
    // no extra arguments.
    { id: HARNESS_ID, harness_id: HARNESS_ID, harness_name: 'Codex', provider_name: 'OpenAI', models: [], efforts: [], route_attached: false, manual_model: false, supports_model: false, supports_extra_args: false },
    { id: `${HARNESS_ID}:minimax`, harness_id: HARNESS_ID, harness_name: 'Codex', provider_name: 'MiniMax', models: [{ id: 'MiniMax-M3', name: 'MiniMax M3', surface: 'openai', efforts: ['none', 'high'] }], efforts: ['none', 'high'], route, route_attached: false, manual_model: false, default_model: 'MiniMax-M3', supports_model: true, supports_extra_args: true },
  ],
  verify_launch_configuration: [],
};

test.describe('spawn configuration submenu placement', () => {
  test.use({ viewport: { width: 900, height: 600 } });

  test('keeps the submenu inside the viewport when the editor grows', async ({ page }) => {
    await page.addInitScript({ content: buildInitScript(fixtures) });
    await page.goto('/');

    const card = page.locator('[data-mesh-card]').last();
    await card.scrollIntoViewIfNeeded();
    await card.getByTestId('spawn-dropdown-toggle').click();

    // The last harness row is the one pinned to the bottom of the list, so
    // its submenu has to open upwards.
    const row = page.locator(`[data-spawn-id="${HARNESS_ID}"]`).last();
    await row.hover();

    const panel = page.getByTestId('spawn-configurations');
    await expect(panel).toBeVisible();
    await page.getByRole('menuitem', { name: `Clone ${savedConfiguration.name}` }).click();

    const editor = page.locator('form[aria-label="Launch Configuration"]');
    await expect(editor).toBeVisible();
    const rowBox = (await row.boundingBox())!;
    const viewport = page.viewportSize()!;

    // The submenu opened above its row: its bottom edge sits on the row.
    const opened = (await panel.boundingBox())!;
    expect(opened.y + opened.height).toBeGreaterThan(rowBox.y + rowBox.height - 24);

    // Switching to the routed provider adds the endpoint fields, so the
    // editor grows without any of the submenu's props changing.
    await editor.getByLabel('Provider').selectOption(`${HARNESS_ID}:minimax`);
    await expect(editor.getByLabel('Provider endpoint')).toBeVisible();

    // The panel is re-placed for the taller form and stays in the viewport.
    await expect.poll(async () => {
      const box = (await panel.boundingBox())!;
      return Math.round(box.y + box.height);
    }).toBeLessThanOrEqual(viewport.height);

    const grown = (await panel.boundingBox())!;
    expect(grown.y).toBeGreaterThanOrEqual(0);
    expect(grown.y + grown.height).toBeLessThanOrEqual(viewport.height);
    // It still hangs off its row rather than drifting to the top of the
    // screen.
    expect(grown.y + grown.height).toBeGreaterThan(rowBox.y + rowBox.height - 24);
  });
});

test.describe('launch configuration pointer drag-and-drop', () => {
  test.use({ viewport: { width: 1400, height: 800 } });

  test('reorders via real pointer drag in SpawnOptionPicker submenu (crossing leadingAction) and in Settings', async ({ page }) => {
    const threeConfigs = [
      { ...savedConfiguration, id: 'launch/sol-fast', name: 'Sol Fast' },
      { ...savedConfiguration, id: 'launch/sol-deep', name: 'Sol Deep' },
      { ...savedConfiguration, id: 'launch/sol-review', name: 'Sol Review' },
    ];
    const dragFixtures = {
      ...defaultFixtures,
      list_providers: [providerRows[providerRows.length - 1]],
      list_routing_options: [providerRows[providerRows.length - 1]],
      list_spawn_configurations: threeConfigs,
      get_launch_targets: fixtures.get_launch_targets,
      get_app_preferences: {
        default_provider: null,
        reviewer_provider: null,
        naming_provider: null,
        harness_defaults: {},
        provider_pairings: [],
        spawn_configurations: threeConfigs,
      },
      get_provider_accounts: [],
      get_keyed_first_class_catalog: [],
      get_provider_accounts_with_preferences_keys: [],
      get_provider_pairings: [],
      get_pairing_verifications: [],
      compatible_providers_by_harness: {},
      list_harness_logins: [],
      get_network_status: { lan_exposure_enabled: false, tls_active: false, exposed_interfaces: [] },
      get_coordinator_status: { enabled: false, has_token: false },
    };

    await page.addInitScript({ content: buildInitScript(dragFixtures) });
    await page.goto('/');
    await page.evaluate((initialConfigs) => {
      const w = window as unknown as {
        __BUILDMESH_MOCK__: {
          on: (cmd: string, fn: (args?: unknown) => unknown) => void;
          emit: (event: string, payload?: unknown) => number;
        };
        __reorderLog: Array<{ harnessId: string; configurationIds: string[] }>;
      };
      let current = [...initialConfigs];
      w.__reorderLog = [];
      w.__BUILDMESH_MOCK__.on('list_spawn_configurations', () => current);
      w.__BUILDMESH_MOCK__.on('reorder_spawn_configurations', (rawArgs) => {
        const args = rawArgs as { harnessId: string; configurationIds: string[] };
        w.__reorderLog.push(args);
        const byId = new Map(current.map((c) => [c.id, c]));
        const next = args.configurationIds
          .map((id) => byId.get(id))
          .filter((c): c is (typeof initialConfigs)[number] => Boolean(c));
        const remaining = current.filter((c) => !args.configurationIds.includes(c.id));
        current = [...next, ...remaining];
        w.__BUILDMESH_MOCK__.emit('provider-list-changed', null);
        return null;
      });
    }, threeConfigs);

    // Open App Settings -> Providers -> Default provider (SpawnOptionPicker has a leadingAction).
    const openSettings = page.getByRole('button', { name: 'Open settings' });
    if (await openSettings.isVisible()) {
      await openSettings.click();
    } else {
      await page.getByRole('button', { name: 'More tools' }).click();
      await page.getByTestId('titlebar-overflow-settings').click();
    }
    await page.getByRole('tab', { name: 'Providers' }).click();
    await page.getByRole('button', { name: 'Default provider', exact: true }).click();

    const pickerMenu = page.getByTestId('spawn-option-picker-menu');
    await expect(pickerMenu).toBeVisible();
    const leadingAction = pickerMenu.locator('[data-spawn-id="__inherit"]');
    await expect(leadingAction).toBeVisible();

    await pickerMenu.locator(`[data-spawn-id="${HARNESS_ID}"]`).hover();
    const submenu = page.getByTestId('spawn-configurations');
    await expect(submenu).toBeVisible();

    const firstHandle = submenu.getByRole('menuitem', { name: 'Reorder Sol Fast', exact: true });
    const lastRow = submenu.getByRole('menuitem', { name: 'Sol Review', exact: true });
    const handleBox = (await firstHandle.boundingBox())!;
    const leadingBox = (await leadingAction.boundingBox())!;
    const lastBox = (await lastRow.boundingBox())!;

    // Pointer drag: press on handle, move past 4px threshold, sweep across leadingAction
    // (must NOT close/unmount the submenu), then drop on Sol Review.
    await page.mouse.move(handleBox.x + handleBox.width / 2, handleBox.y + handleBox.height / 2);
    await page.mouse.down();
    await page.mouse.move(handleBox.x + handleBox.width / 2, handleBox.y + handleBox.height / 2 + 10, { steps: 3 });
    await page.mouse.move(leadingBox.x + leadingBox.width / 2, leadingBox.y + leadingBox.height / 2, { steps: 5 });
    await expect(submenu).toBeVisible();
    await page.mouse.move(lastBox.x + lastBox.width / 2, lastBox.y + lastBox.height / 2 + 4, { steps: 8 });
    await page.mouse.up();

    await expect(submenu).toBeVisible();
    await expect.poll(async () => page.evaluate(() => (window as unknown as { __reorderLog?: Array<{ harnessId: string; configurationIds: string[] }> }).__reorderLog ?? [])).toEqual([
      { harnessId: HARNESS_ID, configurationIds: ['launch/sol-deep', 'launch/sol-review', 'launch/sol-fast'] },
    ]);

    // Dismiss picker, then verify real pointer drag in Settings -> Launch Configurations.
    await page.keyboard.press('Escape');
    await page.keyboard.press('Escape');
    await page.getByRole('tab', { name: 'Launch Configurations' }).click();
    await page.getByRole('button', { name: 'Show launch configurations' }).click();

    const group = page.getByTestId(`launch-config-group-${HARNESS_ID}`);
    await expect(group).toBeVisible();
    await expect(group.locator('li p')).toHaveText([
      'Sol Deep · User-owned',
      'Sol Review · User-owned',
      'Sol Fast · User-owned',
    ]);
    const settingsHandle = group.getByRole('button', { name: 'Reorder Sol Deep under Codex' });
    const settingsTarget = group.getByTestId('launch-config-row-launch/sol-fast');
    const shBox = (await settingsHandle.boundingBox())!;
    const stBox = (await settingsTarget.boundingBox())!;

    await page.mouse.move(shBox.x + shBox.width / 2, shBox.y + shBox.height / 2);
    await page.mouse.down();
    await page.mouse.move(shBox.x + shBox.width / 2, stBox.y + stBox.height / 2 + 4, { steps: 10 });
    await page.mouse.up();

    await expect.poll(async () => page.evaluate(() => (window as unknown as { __reorderLog?: Array<{ harnessId: string; configurationIds: string[] }> }).__reorderLog?.length ?? 0)).toBe(2);
    const secondCall = await page.evaluate(() => (window as unknown as { __reorderLog?: Array<{ harnessId: string; configurationIds: string[] }> }).__reorderLog?.[1]);
    expect(secondCall).toEqual({
      harnessId: HARNESS_ID,
      configurationIds: ['launch/sol-review', 'launch/sol-fast', 'launch/sol-deep'],
    });
  });
});
