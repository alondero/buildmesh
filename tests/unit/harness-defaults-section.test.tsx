/**
 * Settings → Harnesses → Agent Harness defaults: application-level model +
 * effort + permission-mode defaults per Agent Harness (issue #1150 /
 * #1148, permission in #2151). Pinned contract:
 *
 *   * Capability gating — a harness with `supports_model_override = false`
 *     AND `EffortControlKind::None` renders the no-configurable-defaults
 *     state (no input, no select).
 *   * Effort choices and guidance match each harness's declared vocabulary.
 *   * Permission select offers the harness's own modes in its own words
 *     and commits via `set_harness_default` on blur; the effective line
 *     names the mode the next spawn uses. A single enforced mode renders
 *     as read-only text; a harness with no flag renders the written
 *     "no such flag" line instead of a control.
 *   * Save commits via `set_harness_default` on blur (mirrors the existing
 *     autopilot-pool commit pattern); a failed save rolls the draft back
 *     to the last confirmed value so the visible card never lies about
 *     what's stored.
 *   * Reset clears via `clear_harness_default` and removes the row from
 *     the local map immediately so the card collapses back to its
 *     no-default state.
 *   * Hydration — a stored `harness_defaults` map renders with each
 *     harness's controls pre-filled from the stored values.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

/** Capability contract fixtures — minimal `ProviderInfo` rows with the
 *  descriptor fields the section reads. Avoids coupling to the full
 *  ProviderInfo shape used by the Spawn Menu. */
function capsFixture(harness_id: string, opts: {
  supports_model: boolean;
  effortKind: 'none' | 'closed' | 'inline_config';
  effortAllowed?: string[];
  effortKey?: string;
  // Issue #2151: permission modes in the harness's own words. Omitted
  // (or empty) means the harness has no permission flag.
  permissionModes?: Array<{ id: string; label: string; description: string }>;
}): Pick<ProviderInfo, 'capabilities'>['capabilities'] {
  const effort_control =
    opts.effortKind === 'none'
      ? ({ kind: 'none' as const })
      : opts.effortKind === 'closed'
        ? ({ kind: 'closed' as const, allowed: opts.effortAllowed ?? [] })
        : ({ kind: 'inline_config' as const, key: opts.effortKey ?? 'model_reasoning_effort', allowed: opts.effortAllowed ?? [] });
  const permission_modes = opts.permissionModes ?? [];
  return {
    harness_id,
    supports_resume: false,
    auto_resume_on_startup: false,
    requires_attention_hook: false,
    produces_readable_transcript: false,
    supports_model_override: opts.supports_model,
    supports_effort_override: opts.effortKind !== 'none',
    supports_prefill: false,
    is_plain_terminal: harness_id === 'terminal',
    effort_control,
    permission_modes,
    default_permission_mode: permission_modes[0]?.id ?? null,
    available_on: ['windows'],
  } as Pick<ProviderInfo, 'capabilities'>['capabilities'];
}

function providerFixture(
  id: string,
  harness_id: string,
  capabilities: ReturnType<typeof capsFixture>,
  is_proxied = false,
): ProviderInfo {
  return {
    id,
    label: id.charAt(0).toUpperCase() + id.slice(1),
    color: '',
    icon: '',
    resumable: false,
    harness_id,
    provider_id: is_proxied ? 'minimax' : null,
    is_proxied,
    group_key: harness_id,
    capabilities,
  };
}

const CLAUDE_MODES = [
  { id: 'unattended', label: '--dangerously-skip-permissions', description: 'Prompts off.' },
  { id: 'prompt', label: 'Prompts on (no flag)', description: 'Claude Code asks.' },
];
const CLAUDE_ROW = providerFixture(
  'claude',
  'claude',
  capsFixture('claude', { supports_model: true, effortKind: 'closed', effortAllowed: ['low', 'medium', 'high', 'xhigh', 'max'], permissionModes: CLAUDE_MODES }),
);
const CODEX_ROW = providerFixture(
  'codex',
  'codex',
  capsFixture('codex', { supports_model: true, effortKind: 'inline_config', effortAllowed: ['none', 'low', 'medium', 'high', 'xhigh'], permissionModes: [
    { id: 'unattended', label: '--ask-for-approval never', description: 'Approvals off.' },
    { id: 'prompt', label: 'Prompts on (no flag)', description: 'Codex asks.' },
  ] }),
);
const MCODE_ROW = providerFixture(
  'mcode',
  'mcode',
  capsFixture('mcode', { supports_model: false, effortKind: 'none', permissionModes: [
    { id: 'unattended', label: 'Full Access (permissionMode: bypassPermissions)', description: 'Pinned in config.yaml.' },
  ] }),
);
const AGY_MODES = [
  { id: 'unattended', label: '--dangerously-skip-permissions', description: 'Prompts off (today’s behavior).' },
  { id: 'prompt', label: 'Prompts on (no flag)', description: 'Antigravity asks for approval.' },
];
const AGY_ROW = providerFixture(
  'agy',
  'agy',
  capsFixture('agy', { supports_model: true, effortKind: 'none', permissionModes: AGY_MODES }),
);
const OPENCODE_MODES = [
  { id: 'unattended', label: '--auto', description: 'Auto-approve (today’s behavior).' },
  { id: 'prompt', label: 'Prompts on (no flag)', description: 'OpenCode asks for approval.' },
];
const OPENCODE_ROW = providerFixture(
  'opencode',
  'opencode',
  capsFixture('opencode', { supports_model: false, effortKind: 'none', permissionModes: OPENCODE_MODES }),
);
// Kimi Code genuinely has no permission flag and supports model override —
// the truthful fixture for the no-such-flag line (review round 1: AGY and
// OpenCode both ship two modes, so they can never render that line).
const KIMI_ROW = providerFixture(
  'kimi',
  'kimi',
  capsFixture('kimi', { supports_model: true, effortKind: 'none' }),
);
const TERMINAL_ROW = providerFixture(
  'terminal',
  'terminal',
  capsFixture('terminal', { supports_model: false, effortKind: 'none' }),
);

function mockBackend(opts: {
  defaults?: Record<string, { model: string | null; effort: string | null; permission_mode?: string | null }>;
  providers?: ProviderInfo[];
  failOnSave?: boolean;
} = {}) {
  const defaults = opts.defaults ?? {};
  const providers = opts.providers ?? [CLAUDE_ROW, CODEX_ROW, AGY_ROW, OPENCODE_ROW, TERMINAL_ROW];
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: null,
          minimax_api_key: null,
          circuit_agent_pool_size: null,
          harness_defaults: defaults,
        });
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'get_network_status':
        return Promise.resolve({
          lan_exposure_enabled: false,
          port: 1992,
          tls_active: false,
          exposed_interfaces: [],
        });
      case 'list_providers':
        return Promise.resolve(providers);
      case 'get_provider_accounts':
      case 'get_provider_pairings':
      case 'get_pairing_verifications':
      case 'get_keyed_first_class_catalog':
      case 'compatible_providers_for_harness':
      case 'list_device_sessions':
        return Promise.resolve([]);
      case 'set_harness_default':
        if (opts.failOnSave) {
          return Promise.reject(new Error('effort \'bogus\' is not allowed for harness \'claude\''));
        }
        return Promise.resolve();
      case 'clear_harness_default':
        if (opts.failOnSave) {
          return Promise.reject(new Error('backend rejected reset'));
        }
        return Promise.resolve();
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

describe('Settings — Agent Harness defaults', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('renders one card per unique native harness, deduping proxied rows', async () => {
    // A Proxied Provider row that shares a harness_id with the native row
    // must not produce a second card — the harness capability is shared.
    const proxied_claude = providerFixture(
      'claude:minimax',
      'claude',
      CLAUDE_ROW.capabilities,
      true,
    );
    const calls = mockBackend({ providers: [CLAUDE_ROW, proxied_claude, CODEX_ROW] });
    render(<AppSettingsModal onClose={() => {}} />);

    // Wait for hydration; Claude's card is identifiable by its model input.
    await screen.findByTestId('harness-default-claude');
    await screen.findByTestId('harness-default-codex');

    // The proxied row must not produce a duplicate harness card.
    const claudeCards = screen.getAllByTestId('harness-default-claude');
    expect(claudeCards).toHaveLength(1);
    expect(calls['set_harness_default']).toBeUndefined();
  });

  it('renders capability-gated controls — Claude has both, Agy has model + permission, OpenCode has permission only', async () => {
    mockBackend({ providers: [CLAUDE_ROW, AGY_ROW, OPENCODE_ROW, CODEX_ROW, TERMINAL_ROW] });
    render(<AppSettingsModal onClose={() => {}} />);

    // Claude: model input + effort select.
    await screen.findByTestId('harness-default-model-input-claude');
    await screen.findByTestId('harness-default-effort-select-claude');

    // Agy: model input, no effort select, permission select (two real modes).
    await screen.findByTestId('harness-default-model-input-agy');
    expect(screen.queryByTestId('harness-default-effort-select-agy')).toBeNull();
    await screen.findByTestId('harness-permission-select-agy');

    // OpenCode: no model/effort controls and no "no configurable
    // defaults" state — it has a real permission flag, so it renders the
    // permission select instead.
    expect(screen.queryByTestId('harness-default-empty-opencode')).toBeNull();
    expect(screen.queryByTestId('harness-default-model-input-opencode')).toBeNull();
    expect(screen.queryByTestId('harness-default-effort-select-opencode')).toBeNull();
    await screen.findByTestId('harness-permission-select-opencode');
  });

  it('effort choices and guidance match each harness\'s declared vocabulary', async () => {
    mockBackend({ providers: [CLAUDE_ROW, CODEX_ROW] });
    render(<AppSettingsModal onClose={() => {}} />);

    const claudeSelect = await screen.findByTestId<HTMLSelectElement>('harness-default-effort-select-claude');
    const claudeOptions = Array.from(claudeSelect.querySelectorAll('option')).map((o) => o.value);
    expect(claudeOptions).toEqual(['', 'low', 'medium', 'high', 'xhigh', 'max']);
    const claudeHelpId = claudeSelect.getAttribute('aria-describedby');
    expect(claudeHelpId).toBe('harness-default-effort-help-claude');
    expect(document.getElementById(claudeHelpId!)?.textContent).toBe('Availability depends on the selected model.');
    const user = userEvent.setup();
    await user.selectOptions(claudeSelect, 'max');
    await waitFor(() => expect(document.getElementById(claudeHelpId!)?.textContent)
      .toBe('Availability depends on the selected model. Max applies to the launched session.'));

    const codexSelect = await screen.findByTestId<HTMLSelectElement>('harness-default-effort-select-codex');
    const codexOptions = Array.from(codexSelect.querySelectorAll('option')).map((o) => o.value);
    // Codex's vocabulary: superset that includes xhigh.
    expect(codexOptions).toEqual(['', 'none', 'low', 'medium', 'high', 'xhigh']);
    expect(codexSelect.getAttribute('aria-describedby')).toBeNull();
  });

  it('hydrates the model + effort fields from the stored harness_defaults map', async () => {
    mockBackend({
      defaults: {
        claude: { model: 'opus-4-1', effort: 'high' },
        codex: { model: null, effort: 'xhigh' },
      },
      providers: [CLAUDE_ROW, CODEX_ROW],
    });
    render(<AppSettingsModal onClose={() => {}} />);

    const claudeModel = await screen.findByTestId<HTMLInputElement>('harness-default-model-input-claude');
    await waitFor(() => expect(claudeModel.value).toBe('opus-4-1'));

    const claudeEffort = await screen.findByTestId<HTMLSelectElement>('harness-default-effort-select-claude');
    await waitFor(() => expect(claudeEffort.value).toBe('high'));

    const codexEffort = await screen.findByTestId<HTMLSelectElement>('harness-default-effort-select-codex');
    await waitFor(() => expect(codexEffort.value).toBe('xhigh'));
  });

  it('commits a typed model value via set_harness_default on blur', async () => {
    const calls = mockBackend();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await screen.findByTestId<HTMLInputElement>('harness-default-model-input-claude');
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'opus-4-1');
    // No save until blur (mirrors the autopilot-pool commit-on-blur).
    expect(calls['set_harness_default']).toBeUndefined();
    input.blur();

    await waitFor(() => expect(calls['set_harness_default']).toBeTruthy());
    const last = calls['set_harness_default']!.at(-1);
    expect(last).toEqual({
      profileId: 'claude',
      value: { model: 'opus-4-1', effort: null, permission_mode: null },
    });
  });

  it('failed save rolls the draft back to the last confirmed value', async () => {
    mockBackend({ failOnSave: true });
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await screen.findByTestId<HTMLInputElement>('harness-default-model-input-claude');
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'opus-4-1');
    input.blur();

    // Wait for the rolled-back input. The draft returns to empty (the
    // last confirmed value is `null` — the harness had no stored default
    // before this attempt).
    await waitFor(() => expect(input.value).toBe(''));
    // The shared error banner is visible.
    await screen.findByText(/effort 'bogus' is not allowed/);
  });

  it('reset clears via clear_harness_default and collapses the card back to empty', async () => {
    const calls = mockBackend({
      defaults: { claude: { model: 'opus-4-1', effort: 'high' } },
      providers: [CLAUDE_ROW],
    });
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const resetButton = await screen.findByTestId('harness-default-reset-claude');
    await user.click(resetButton);

    await waitFor(() => expect(calls['clear_harness_default']).toBeTruthy());
    expect(calls['clear_harness_default']!.at(-1)).toEqual({ profileId: 'claude' });

    // The card collapses back to empty after the clear: the Reset
    // button disappears (no stored default left) and the model input
    // clears. The local map update is optimistic (the parent's
    // `handleClearHarnessDefault` deletes the key before the IPC
    // resolves), so the assertion fires immediately after the click.
    await waitFor(() =>
      expect(screen.queryByTestId('harness-default-reset-claude')).toBeNull(),
    );
  });

  it('Terminal renders the no-configurable-defaults state', async () => {
    mockBackend({ providers: [TERMINAL_ROW] });
    render(<AppSettingsModal onClose={() => {}} />);
    const empty = await screen.findByTestId('harness-default-empty-terminal');
    expect(empty.textContent).toMatch(/does not accept model, effort, or permission-mode overrides/i);
  });

  it('permission select offers the harness modes in its own words and commits on blur', async () => {
    const calls = mockBackend({ providers: [CLAUDE_ROW] });
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const select = await screen.findByTestId<HTMLSelectElement>('harness-permission-select-claude');
    const options = Array.from(select.querySelectorAll('option')).map((o) => o.value);
    expect(options).toEqual(['', 'unattended', 'prompt']);
    // The harness-words label is the visible text of the default option.
    expect(select.querySelector('option[value="unattended"]')?.textContent)
      .toBe('--dangerously-skip-permissions');
    // No stored value: the effective line names the harness default.
    const effective = await screen.findByTestId('harness-permission-effective-claude');
    expect(effective.textContent).toMatch(/Effective: --dangerously-skip-permissions \(harness default\)/);

    await user.selectOptions(select, 'prompt');
    select.blur();

    await waitFor(() => expect(calls['set_harness_default']).toBeTruthy());
    const last = calls['set_harness_default']!.at(-1);
    expect(last).toEqual({
      profileId: 'claude',
      value: { model: null, effort: null, permission_mode: 'prompt' },
    });
    // After commit the effective line follows the stored choice.
    await waitFor(() => expect(screen.getByTestId('harness-permission-effective-claude').textContent)
      .toMatch(/Effective: Prompts on \(no flag\)/));
  });

  it('hydrated permission default preselects the stored mode as custom', async () => {
    mockBackend({
      defaults: { codex: { model: null, effort: null, permission_mode: 'prompt' } },
      providers: [CODEX_ROW],
    });
    render(<AppSettingsModal onClose={() => {}} />);

    const select = await screen.findByTestId<HTMLSelectElement>('harness-permission-select-codex');
    await waitFor(() => expect(select.value).toBe('prompt'));
    const effective = await screen.findByTestId('harness-permission-effective-codex');
    // A stored choice renders without the "(harness default)" marker.
    expect(effective.textContent).toMatch(/Effective: Prompts on \(no flag\)/);
    expect(effective.textContent).not.toMatch(/harness default/);
  });

  it('single enforced mode renders as read-only text, not a select', async () => {
    mockBackend({ providers: [MCODE_ROW] });
    render(<AppSettingsModal onClose={() => {}} />);

    const fixed = await screen.findByTestId('harness-permission-fixed-mcode');
    expect(fixed.textContent).toBe('Full Access (permissionMode: bypassPermissions)');
    expect(screen.queryByTestId('harness-permission-select-mcode')).toBeNull();
    // No second "Effective:" line — the fixed label already names the mode.
    expect(screen.queryByTestId('harness-permission-effective-mcode')).toBeNull();
  });

  it('harness with model control but no flag renders the no-such-flag line', async () => {
    // Kimi Code genuinely ships no permission flag (unlike Agy/OpenCode,
    // which both offer unattended + prompt).
    mockBackend({ providers: [KIMI_ROW] });
    render(<AppSettingsModal onClose={() => {}} />);

    // Kimi keeps its model input (unchanged behavior) …
    await screen.findByTestId('harness-default-model-input-kimi');
    // … and gains the written "no such flag" line instead of a control.
    const line = await screen.findByTestId('harness-permission-noflag-kimi');
    expect(line.textContent).toMatch(/has no permission flag/i);
    expect(screen.queryByTestId('harness-permission-select-kimi')).toBeNull();
  });
});
