import { describe, it, expect, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { HarnessConfigList } from '../../src/components/AppSettings/HarnessConfigList';
import type { ProviderAccount, ProviderPairing } from '../../src/lib/tauri';

const NO_TIERS = { default: null, small_fast: null, sonnet: null, opus: null, haiku: null };

function account(over: Partial<ProviderAccount> = {}): ProviderAccount {
  return {
    id: 'minimax',
    name: 'MiniMax',
    enabled: true,
    billing_mode: 'pay_as_you_go',
    claude_compatible: true,
    api_key: 'sk-mm',
    base_url: null,
    model_tiers: NO_TIERS,
    models: [],
    ...over,
  };
}

function pairing(over: Partial<ProviderPairing> = {}): ProviderPairing {
  return {
    harness_id: 'claude',
    provider_id: 'minimax',
    surface: 'anthropic',
    base_url: 'https://api.minimax.io/anthropic',
    model_tiers: NO_TIERS,
    ...over,
  };
}

const harnesses = [
  { id: 'claude', label: 'Claude Code' },
  { id: 'codex', label: 'OpenAI Codex' },
];

describe('HarnessConfigList (issue #576)', () => {
  it('shows attached pairings with their surface and endpoint, grouped by harness', () => {
    render(
      <HarnessConfigList
        harnesses={harnesses}
        compatibleByHarness={{ claude: [account()], codex: [account()] }}
        pairings={[pairing()]}
        storedKeys={new Set()}
        accounts={[account()]}
        onAttach={vi.fn()}
        onDetach={vi.fn()}
      />,
    );
    const claudeCard = screen.getByTestId('harness-claude');
    expect(within(claudeCard).getByText('MiniMax')).toBeTruthy();
    expect(within(claudeCard).getByText('Anthropic')).toBeTruthy();
    expect(within(claudeCard).getByText('https://api.minimax.io/anthropic')).toBeTruthy();
    // Codex card has no attached pairing.
    expect(within(screen.getByTestId('harness-codex')).getByText(/no proxied providers attached/i)).toBeTruthy();
  });

  it('marks a derived default pairing as managed-on-Providers (no Detach)', () => {
    render(
      <HarnessConfigList
        harnesses={harnesses}
        compatibleByHarness={{ claude: [account()] }}
        pairings={[pairing()]}
        storedKeys={new Set()} // derived default — not stored
        accounts={[account()]}
        onAttach={vi.fn()}
        onDetach={vi.fn()}
      />,
    );
    expect(screen.getByText(/default · key on providers/i)).toBeTruthy();
    expect(screen.queryByRole('button', { name: /detach/i })).toBeNull();
  });

  it('detaches a user-stored pairing', async () => {
    const onDetach = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <HarnessConfigList
        harnesses={harnesses}
        compatibleByHarness={{ codex: [account()] }}
        pairings={[pairing({ harness_id: 'codex', surface: 'openai', base_url: 'https://api.minimax.io/v1' })]}
        storedKeys={new Set(['codex:minimax'])}
        accounts={[account()]}
        onAttach={vi.fn()}
        onDetach={onDetach}
      />,
    );
    await user.click(screen.getByRole('button', { name: /detach minimax from openai codex/i }));
    await waitFor(() => expect(onDetach).toHaveBeenCalledWith('codex', 'minimax'));
  });

  it('offers only compatible providers not already attached', async () => {
    const user = userEvent.setup();
    const deepseek = account({ id: 'deepseek', name: 'DeepSeek' });
    render(
      <HarnessConfigList
        harnesses={[{ id: 'claude', label: 'Claude Code' }]}
        compatibleByHarness={{ claude: [account(), deepseek] }}
        pairings={[pairing()]} // minimax already attached to claude
        storedKeys={new Set(['claude:minimax'])}
        accounts={[account(), deepseek]}
        onAttach={vi.fn()}
        onDetach={vi.fn()}
      />,
    );
    await user.click(screen.getByRole('button', { name: /add proxied provider/i }));
    const select = screen.getByRole('combobox', { name: /provider to attach to claude code/i });
    const optionNames = within(select).getAllByRole('option').map((o) => o.textContent);
    // MiniMax is already attached → excluded; DeepSeek offered.
    expect(optionNames).toContain('DeepSeek');
    expect(optionNames).not.toContain('MiniMax');
  });

  it('attaches a keyed provider without prompting for a key', async () => {
    const onAttach = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    const deepseek = account({ id: 'deepseek', name: 'DeepSeek', api_key: 'sk-deep' });
    render(
      <HarnessConfigList
        harnesses={[{ id: 'codex', label: 'OpenAI Codex' }]}
        compatibleByHarness={{ codex: [deepseek] }}
        pairings={[]}
        storedKeys={new Set()}
        accounts={[deepseek]}
        onAttach={onAttach}
        onDetach={vi.fn()}
      />,
    );
    await user.click(screen.getByRole('button', { name: /add proxied provider/i }));
    await user.selectOptions(screen.getByRole('combobox', { name: /provider to attach/i }), 'deepseek');
    // Already keyed → no key field.
    expect(screen.queryByLabelText(/deepseek api key/i)).toBeNull();
    await user.click(screen.getByRole('button', { name: /^attach$/i }));
    await waitFor(() => expect(onAttach).toHaveBeenCalledWith('codex', 'deepseek', null));
  });

  it('requires and forwards an API key when attaching a keyless provider (set-if-absent)', async () => {
    const onAttach = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    const keyless = account({ id: 'minimax', name: 'MiniMax', api_key: null });
    render(
      <HarnessConfigList
        harnesses={[{ id: 'codex', label: 'OpenAI Codex' }]}
        compatibleByHarness={{ codex: [keyless] }}
        pairings={[]}
        storedKeys={new Set()}
        accounts={[keyless]}
        onAttach={onAttach}
        onDetach={vi.fn()}
      />,
    );
    await user.click(screen.getByRole('button', { name: /add proxied provider/i }));
    await user.selectOptions(screen.getByRole('combobox', { name: /provider to attach/i }), 'minimax');
    // Keyless → Attach is gated until a key is entered.
    const attach = screen.getByRole('button', { name: /^attach$/i }) as HTMLButtonElement;
    expect(attach.disabled).toBe(true);
    await user.type(screen.getByLabelText(/minimax api key/i), 'sk-mm');
    await user.click(attach);
    await waitFor(() => expect(onAttach).toHaveBeenCalledWith('codex', 'minimax', 'sk-mm'));
  });
});
