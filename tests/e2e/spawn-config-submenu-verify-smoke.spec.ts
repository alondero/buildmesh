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
  supports_extra_args: true,
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
