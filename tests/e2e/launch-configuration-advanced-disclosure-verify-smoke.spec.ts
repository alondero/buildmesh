/**
 * Launch Configuration advanced disclosure — browser regression.
 *
 * Issue #2423 asks the launch configuration editor to progressively disclose
 * its advanced fields instead of laying the inputs out in one continuous
 * form. The rules this spec pins down:
 *
 *  1. A new ordinary setup keeps the optional advanced inputs collapsed behind
 *     a labelled disclosure that explains when they are needed.
 *  2. The disclosure is operable from the keyboard alone.
 *  3. A required advanced value (a custom model reference) stays on screen,
 *     and the disclosure refuses to close over it.
 *  4. The required endpoint and the verification action stay visible outside
 *     the disclosure.
 *
 * The spec drives the real frontend in Chromium against the mocked Tauri IPC
 * (scripts/ui-mock/tauri-mock.mjs) inside the Settings modal at a narrow
 * viewport, so the disclosure runs in the same cramped column a small window
 * gives it.
 *
 * Run standalone: npx playwright test --project=verify-smoke
 */
import { test, expect } from '@playwright/test';
import { buildInitScript, defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

const HARNESS_ID = 'codex';

/**
 * The Settings pane derives its harness list from the provider rows, so the
 * rows need their capability table rather than the bare catalogue shape the
 * default fixtures carry.
 */
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

const providerRows = [{
  id: HARNESS_ID,
  label: HARNESS_ID,
  color: '#00d4ff',
  icon: 'codex',
  resumable: true,
  harness_id: HARNESS_ID,
  provider_id: null,
  is_proxied: false,
  group_key: HARNESS_ID,
  runtime: 'windows',
  capabilities,
}] satisfies ProviderInfo[];

const route = {
  harness_id: HARNESS_ID,
  provider_id: 'minimax',
  surface: 'openai' as const,
  base_url: 'https://api.minimax.io/v1',
  model_tiers: { default: 'MiniMax-M3', opus: null, fable: null, sonnet: null, haiku: null, small_fast: null },
};

const models = [{ id: 'MiniMax-M3', name: 'MiniMax M3', surface: 'openai', efforts: ['none', 'high'] }];

/** The ordinary native target: no route, so no endpoint field. */
const nativeTarget = {
  id: HARNESS_ID,
  harness_id: HARNESS_ID,
  harness_name: 'Codex',
  provider_name: 'OpenAI',
  models,
  efforts: ['none', 'high'],
  route_attached: false,
  manual_model: false,
  default_model: 'MiniMax-M3',
  supports_model: true,
  supports_extra_args: true,
};

/** A proxied target: the endpoint field and the verification action belong to it. */
const proxiedTarget = {
  id: `${HARNESS_ID}:minimax`,
  harness_id: HARNESS_ID,
  harness_name: 'Codex',
  provider_name: 'MiniMax',
  models,
  efforts: ['none', 'high'],
  route,
  route_attached: false,
  manual_model: false,
  default_model: 'MiniMax-M3',
  supports_model: true,
  supports_extra_args: true,
  verification_required: true,
};

const fixtures = {
  ...defaultFixtures,
  list_providers: providerRows,
  list_spawn_configurations: [],
  get_launch_targets: [nativeTarget, proxiedTarget],
  verify_launch_configuration: [],
};

/** Open the Settings modal and the Launch Configuration editor it owns. */
async function openEditor(page: import('@playwright/test').Page) {
  await page.addInitScript({ content: buildInitScript(fixtures) });
  await page.goto('/');
  await page.getByRole('button', { name: 'Open settings', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
  await expect(dialog).toBeVisible();
  await dialog.getByRole('tab', { name: 'Launch Configurations' }).click();
  await dialog.getByRole('button', { name: 'Show launch configurations' }).click();
  await dialog.getByRole('button', { name: 'New Launch Configuration' }).click();
  const editor = dialog.locator('form[aria-label="Launch Configuration"]');
  await expect(editor).toBeVisible();
  return editor;
}

test.describe('launch configuration advanced disclosure', () => {
  test.use({ viewport: { width: 460, height: 900 } });

  test('collapses the optional advanced inputs for a new ordinary setup', async ({ page }) => {
    const editor = await openEditor(page);
    const toggle = editor.getByRole('button', { name: 'Advanced options' });

    // The common choices stay visible.
    await expect(editor.getByLabel('Name')).toBeVisible();
    await expect(editor.getByLabel('Harness')).toBeVisible();
    await expect(editor.getByLabel('Provider')).toBeVisible();
    await expect(editor.getByLabel('Model')).toBeVisible();
    await expect(editor.getByLabel('Effort')).toBeVisible();

    // The optional advanced input is collapsed, and the disclosure says when it
    // is needed.
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await expect(editor.getByLabel('Extra arguments')).toBeHidden();
    await expect(editor.getByText(/Only needed when a provider or harness wants a raw value/)).toBeVisible();
  });

  test('opens and closes the disclosure from the keyboard', async ({ page }) => {
    const editor = await openEditor(page);
    const toggle = editor.getByRole('button', { name: 'Advanced options' });

    await toggle.focus();
    await expect(toggle).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect(editor.getByLabel('Extra arguments')).toBeVisible();

    await page.keyboard.press('Space');
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await expect(editor.getByLabel('Extra arguments')).toBeHidden();
  });

  test('keeps a required custom model visible and refuses to hide it', async ({ page }) => {
    const editor = await openEditor(page);
    const toggle = editor.getByRole('button', { name: 'Advanced options' });

    await editor.getByLabel('Model').selectOption('__custom__');

    // The required raw model is on screen with the disclosure open.
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    const customModel = editor.getByRole('textbox', { name: 'Custom model' });
    await expect(customModel).toBeVisible();
    await expect(customModel).toHaveAttribute('required', '');

    // Closing over a required field is refused, and the reason is announced.
    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect(editor.getByRole('alert')).toContainText('custom model is required');
    await expect(customModel).toBeVisible();
  });

  test('keeps the required endpoint and the verification action outside the disclosure', async ({ page }) => {
    const editor = await openEditor(page);
    const toggle = editor.getByRole('button', { name: 'Advanced options' });

    await editor.getByLabel('Provider').selectOption(proxiedTarget.id);
    await expect(editor.getByLabel('Provider endpoint')).toBeVisible();
    await expect(editor.getByRole('button', { name: 'Verify provider and model' })).toBeVisible();

    // The required fields stay outside the collapsed disclosure.
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    const controls = await toggle.getAttribute('aria-controls');
    expect(controls).toBeTruthy();
    const inside = await page.evaluate((id) => {
      const region = document.getElementById(id!);
      if (!region) return true;
      const text = region.textContent ?? '';
      return text.includes('Provider endpoint') || text.includes('Verify provider and model');
    }, controls);
    expect(inside).toBe(false);
  });
});
