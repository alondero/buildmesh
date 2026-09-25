/**
 * SpawnOptionPicker — the Spawn Menu reused as a settings control.
 *
 * Pins the two contracts the settings surfaces rely on:
 *   - picking a native harness row stores its Spawn Option id;
 *   - picking a saved Launch Configuration (from the harness's `›` submenu)
 *     stores the *configuration* id, so a default can point at a recipe.
 * Plus the inherit/unset row and the current-selection label.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { SpawnOptionPicker } from '../../src/components/Providers/SpawnOptionPicker';
import type { ProviderInfo } from '../../src/lib/tauri';

function provider(id: string, label: string, harnessId: string, caps: Partial<Record<string, unknown>> = {}): ProviderInfo {
  return {
    id,
    label,
    color: '#fff',
    icon: id,
    resumable: false,
    harness_id: harnessId,
    provider_id: null,
    is_proxied: false,
    group_key: harnessId,
    capabilities: {
      harness_id: harnessId,
      supports_resume: false,
      auto_resume_on_startup: false,
      requires_attention_hook: false,
      produces_readable_transcript: false,
      supports_model_override: false,
      supports_effort_override: false,
      supports_prefill: false,
      is_plain_terminal: false,
      effort_control: { kind: 'none' },
      available_on: ['windows'],
      ...caps,
    },
  } as ProviderInfo;
}

const PROVIDERS: ProviderInfo[] = [
  provider('claude', 'Claude Code', 'claude', { supports_model_override: true }),
  provider('codex', 'Codex', 'codex'),
];

const CONFIGURATION = {
  id: 'launch/fast',
  name: 'Fast',
  spawn_option_id: 'claude',
  model: 'opus-4',
  effort: 'high',
  extra_args: null,
};

beforeEach(() => {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'list_spawn_configurations':
        return Promise.resolve([CONFIGURATION]);
      case 'get_launch_targets':
        return Promise.resolve([]);
      case 'list_providers':
        return Promise.resolve([]);
      default:
        return Promise.resolve({});
    }
  });
});

describe('SpawnOptionPicker', () => {
  it('shows the unset label when no value is selected', () => {
    render(
      <SpawnOptionPicker
        ariaLabel="Default provider"
        providers={PROVIDERS}
        value={null}
        unsetLabel="<Default> (Claude Code)"
        unsetValue=""
        onSelect={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Default provider' }).textContent).toContain(
      '<Default> (Claude Code)',
    );
  });

  it('shows the label of the selected harness', () => {
    render(
      <SpawnOptionPicker
        ariaLabel="Default provider"
        providers={PROVIDERS}
        value="codex"
        unsetLabel="<Default>"
        unsetValue=""
        onSelect={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Default provider' }).textContent).toContain('Codex');
  });

  it('stores a native harness id when a harness row is picked', async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(
      <SpawnOptionPicker
        ariaLabel="Default provider"
        providers={PROVIDERS}
        value={null}
        unsetLabel="<Default>"
        unsetValue=""
        onSelect={onSelect}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Default provider' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Codex' }));

    expect(onSelect).toHaveBeenCalledWith('codex');
  });

  it('stores a configuration id when a saved Launch Configuration is picked', async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(
      <SpawnOptionPicker
        ariaLabel="Default provider"
        providers={PROVIDERS}
        value={null}
        unsetLabel="<Default>"
        unsetValue=""
        onSelect={onSelect}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Default provider' }));
    // The native Claude row exposes the configurations disclosure.
    await user.click(await screen.findByLabelText('Claude Code configurations'));
    // The submenu lists the saved configuration; picking it stores its id.
    await user.click(await screen.findByRole('menuitem', { name: 'Fast' }));

    expect(onSelect).toHaveBeenCalledWith('launch/fast');
  });

  it('clears to the unset value from the inherit row', async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(
      <SpawnOptionPicker
        ariaLabel="Default provider"
        providers={PROVIDERS}
        value="codex"
        unsetLabel="Anthropic (built-in default)"
        unsetValue="__no_override__"
        onSelect={onSelect}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Default provider' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Anthropic (built-in default)' }));

    expect(onSelect).toHaveBeenCalledWith('__no_override__');
  });

  it('excludes filtered harnesses (e.g. Terminal) from the menu', async () => {
    const user = userEvent.setup();
    render(
      <SpawnOptionPicker
        ariaLabel="Reviewer provider"
        providers={[...PROVIDERS, provider('terminal', 'Terminal', 'terminal')]}
        value={null}
        unsetLabel="Source agent provider"
        unsetValue={null}
        filter={(option) => option.harness_id !== 'terminal'}
        onSelect={() => {}}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Reviewer provider' }));
    expect(await screen.findByRole('menuitem', { name: 'Claude Code' })).toBeTruthy();
    expect(screen.queryByRole('menuitem', { name: 'Terminal' })).toBeNull();
  });
});
