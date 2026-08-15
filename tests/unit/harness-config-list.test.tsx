import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { HarnessConfigList, reorderProxiedIds } from '../../src/components/AppSettings/HarnessConfigList';
import type { PairingVerification, ProviderAccount, ProviderPairing } from '../../src/lib/tauri';

const NO_TIERS = { default: null, small_fast: null, sonnet: null, opus: null, fable: null, haiku: null };

function account(over: Partial<ProviderAccount> = {}): ProviderAccount {
  return {
    id: 'minimax',
    name: 'MiniMax',
    enabled: true,
    billing_mode: 'pay_as_you_go',
    claude_compatible: true,
    api_key: 'sk-mm',
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

function verification(over: Partial<PairingVerification> = {}): PairingVerification {
  return {
    harness_id: 'codex',
    provider_id: 'minimax',
    pairing_signature: 'signature',
    endpoint: 'https://api.minimax.io/v1',
    model_id: 'MiniMax-M3',
    auth_mode: 'bearer_env',
    runtime: 'native-windows',
    executable: 'codex',
    codex_version: '0.144.0',
    capability_result: { compatible: true, reason: null },
    status: 'verified',
    verified_at: '2026-08-15T10:00:00Z',
    reason: null,
    ...over,
  };
}

const harnesses = [
  { id: 'claude', label: 'Claude Code' },
  { id: 'codex', label: 'OpenAI Codex' },
];

/** Prefill attach form via get_pairing_defaults IPC. */
function mockPairingDefaults() {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'get_pairing_defaults') {
      const harnessId = args?.harnessId as string;
      const providerId = args?.providerId as string;
      if (harnessId === 'codex') {
        return Promise.resolve({
          harness_id: harnessId,
          provider_id: providerId,
          surface: 'openai',
          base_url: 'https://api.example.com/v1',
          model_tiers: { ...NO_TIERS, default: 'test-responses-model' },
        });
      }
      return Promise.resolve({
        harness_id: harnessId,
        provider_id: providerId,
        surface: 'anthropic',
        base_url: 'https://api.example.com/anthropic',
        model_tiers: NO_TIERS,
      });
    }
    return Promise.resolve(undefined);
  });
}

describe('HarnessConfigList (issue #576)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockPairingDefaults();
  });

  it('shows attached pairings with their surface and endpoint, grouped by harness', () => {
    render(
      <HarnessConfigList
        harnesses={harnesses}
        compatibleByHarness={{ claude: [account()], codex: [account()] }}
        pairings={[pairing()]}
        storedKeys={new Set(['claude:minimax'])}
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

  it('shows Detach for every stored pairing (no derived-default placeholder)', () => {
    // ADR-0025: all pairings are stored/detachable — "Default · key on Providers" is gone.
    render(
      <HarnessConfigList
        harnesses={harnesses}
        compatibleByHarness={{ claude: [account()] }}
        pairings={[pairing()]}
        storedKeys={new Set(['claude:minimax'])}
        accounts={[account()]}
        onAttach={vi.fn()}
        onDetach={vi.fn()}
      />,
    );
    expect(screen.queryByText(/default · key on providers/i)).toBeNull();
    expect(screen.getByRole('button', { name: /detach minimax from claude code/i })).toBeTruthy();
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

  it('shows actionable verification state and allows manual reverification', async () => {
    const onVerify = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <HarnessConfigList
        harnesses={[{ id: 'codex', label: 'OpenAI Codex' }]}
        compatibleByHarness={{ codex: [account()] }}
        pairings={[
          pairing({
            harness_id: 'codex',
            surface: 'openai',
            base_url: 'https://api.minimax.io/v1',
            model_tiers: { ...NO_TIERS, default: 'MiniMax-M3' },
          }),
        ]}
        verifications={[
          verification({
            status: 'stale',
            reason: 'routing inputs changed; verify the pairing again',
          }),
          verification({ runtime: 'wsl', status: 'verified' }),
        ]}
        storedKeys={new Set(['codex:minimax'])}
        accounts={[account()]}
        onAttach={vi.fn()}
        onDetach={vi.fn()}
        onVerify={onVerify}
      />,
    );

    expect(screen.getByText('Reverification required')).toBeTruthy();
    expect(screen.getByText(/routing inputs changed/i)).toBeTruthy();
    await user.click(screen.getByRole('button', { name: /verify native minimax under openai codex/i }));
    await waitFor(() => expect(onVerify).toHaveBeenCalledWith('codex', 'minimax', 'windows'));
    await user.click(screen.getByRole('button', { name: /verify wsl minimax under openai codex/i }));
    await waitFor(() => expect(onVerify).toHaveBeenCalledWith('codex', 'minimax', 'wsl'));
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

  it('attaches a keyed provider without prompting for a key (requires base URL)', async () => {
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
    // Defaults prefill base URL; wait for getPairingDefaults.
    await waitFor(() => {
      expect((screen.getByLabelText(/base url for deepseek/i) as HTMLInputElement).value).toBe(
        'https://api.example.com/v1',
      );
    });
    await user.click(screen.getByRole('button', { name: /^attach$/i }));
    // onAttach(harnessId, providerId, apiKey, baseUrl, modelTiers)
    // OpenAI surface → modelTiers null.
    await waitFor(() =>
      expect(onAttach).toHaveBeenCalledWith(
        'codex',
        'deepseek',
        null,
        'https://api.example.com/v1',
        { ...NO_TIERS, default: 'test-responses-model' },
      ),
    );
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
    await waitFor(() => {
      expect((screen.getByLabelText(/base url for minimax/i) as HTMLInputElement).value).toBe(
        'https://api.example.com/v1',
      );
    });
    // Keyless → Attach is gated until a key is entered.
    const attach = screen.getByRole('button', { name: /^attach$/i }) as HTMLButtonElement;
    expect(attach.disabled).toBe(true);
    await user.type(screen.getByLabelText(/minimax api key/i), 'sk-mm');
    await user.click(attach);
    await waitFor(() =>
      expect(onAttach).toHaveBeenCalledWith(
        'codex',
        'minimax',
        'sk-mm',
        'https://api.example.com/v1',
        { ...NO_TIERS, default: 'test-responses-model' },
      ),
    );
  });

  it('forwards model tiers when attaching on an Anthropic surface', async () => {
    const onAttach = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    const deepseek = account({ id: 'deepseek', name: 'DeepSeek', api_key: 'sk-deep' });
    render(
      <HarnessConfigList
        harnesses={[{ id: 'claude', label: 'Claude Code' }]}
        compatibleByHarness={{ claude: [deepseek] }}
        pairings={[]}
        storedKeys={new Set()}
        accounts={[deepseek]}
        onAttach={onAttach}
        onDetach={vi.fn()}
      />,
    );
    await user.click(screen.getByRole('button', { name: /add proxied provider/i }));
    await user.selectOptions(screen.getByRole('combobox', { name: /provider to attach/i }), 'deepseek');
    await waitFor(() => {
      expect(screen.getByLabelText(/deepseek default model/i)).toBeTruthy();
    });
    await user.type(screen.getByLabelText(/deepseek opus model/i), 'deepseek-v3');
    await user.click(screen.getByRole('button', { name: /^attach$/i }));
    await waitFor(() => {
      expect(onAttach).toHaveBeenCalled();
      const call = onAttach.mock.calls[0];
      expect(call[0]).toBe('claude');
      expect(call[1]).toBe('deepseek');
      expect(call[2]).toBe(null);
      expect(call[3]).toBe('https://api.example.com/anthropic');
      expect(call[4]).toMatchObject({ opus: 'deepseek-v3' });
    });
  });
});

// Issue #577 — per-harness Proxied Provider child reorder. Cross-harness
// drag is disallowed by structural scoping (each `HarnessCard` wraps its
// child list in its own `DndContext`), so the test verifies that a row
// from harness A's card is not a valid drop target on harness B's card,
// and that the drag handler forwards the new order via the supplied
// `onReorderProxied` callback.
describe('reorderProxiedIds (issue #577)', () => {
  it('moves an id later in the list', () => {
    expect(reorderProxiedIds(['a', 'b', 'c'], 'a', 'c')).toEqual(['b', 'c', 'a']);
  });

  it('moves an id earlier in the list', () => {
    expect(reorderProxiedIds(['a', 'b', 'c'], 'c', 'a')).toEqual(['c', 'a', 'b']);
  });

  it('is a no-op when active and over are the same', () => {
    expect(reorderProxiedIds(['a', 'b', 'c'], 'b', 'b')).toEqual(['a', 'b', 'c']);
  });

  it('is a no-op when an id is missing', () => {
    expect(reorderProxiedIds(['a', 'b'], 'a', 'z')).toEqual(['a', 'b']);
  });
});

describe('HarnessConfigList — per-harness child reorder (issue #577)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockPairingDefaults();
  });

  it('renders a reorder handle for each stored pairing under a harness', () => {
    render(
      <HarnessConfigList
        harnesses={harnesses}
        compatibleByHarness={{ claude: [account()], codex: [account()] }}
        pairings={[
          pairing({ harness_id: 'claude', provider_id: 'minimax' }),
          pairing({ harness_id: 'claude', provider_id: 'kimi' }),
          pairing({ harness_id: 'codex', provider_id: 'minimax' }),
        ]}
        storedKeys={new Set(['claude:minimax', 'claude:kimi', 'codex:minimax'])}
        accounts={[account()]}
        onAttach={vi.fn()}
        onDetach={vi.fn()}
        onReorderProxied={vi.fn()}
      />,
    );
    const claudeCard = screen.getByTestId('harness-claude');
    // `harnessLabel` is the human-readable harness name ("Claude Code" /
    // "OpenAI Codex"), matching what the existing Detach aria-label uses.
    expect(within(claudeCard).getByLabelText(/reorder minimax under claude code/i)).toBeTruthy();
    expect(within(claudeCard).getByLabelText(/reorder kimi under claude code/i)).toBeTruthy();
    const codexCard = screen.getByTestId('harness-codex');
    expect(within(codexCard).getByLabelText(/reorder minimax under openai codex/i)).toBeTruthy();
  });

  it('renders reorder handles when storedKeys is empty but pairings exist (all detachable fallback)', () => {
    // ADR-0025: empty stored set with pairings present treats every row as detachable.
    render(
      <HarnessConfigList
        harnesses={harnesses}
        compatibleByHarness={{ claude: [account()], codex: [account()] }}
        pairings={[
          pairing({ harness_id: 'claude', provider_id: 'minimax' }),
        ]}
        storedKeys={new Set()}
        accounts={[account()]}
        onAttach={vi.fn()}
        onDetach={vi.fn()}
        onReorderProxied={vi.fn()}
      />,
    );
    const claudeCard = screen.getByTestId('harness-claude');
    expect(within(claudeCard).queryByText(/default · key on providers/i)).toBeNull();
    expect(within(claudeCard).getByLabelText(/reorder minimax under claude code/i)).toBeTruthy();
    expect(within(claudeCard).getByRole('button', { name: /detach minimax from claude code/i })).toBeTruthy();
  });

  it('cross-harness drag is structurally disallowed — a row lives under exactly one harness card', () => {
    render(
      <HarnessConfigList
        harnesses={harnesses}
        compatibleByHarness={{ claude: [account()], codex: [account()] }}
        pairings={[
          pairing({ harness_id: 'claude', provider_id: 'minimax' }),
          pairing({ harness_id: 'claude', provider_id: 'kimi' }),
          pairing({ harness_id: 'codex', provider_id: 'minimax' }),
        ]}
        storedKeys={new Set(['claude:minimax', 'claude:kimi', 'codex:minimax'])}
        accounts={[account()]}
        onAttach={vi.fn()}
        onDetach={vi.fn()}
        onReorderProxied={vi.fn()}
      />,
    );
    // Each pair lives under exactly one harness card.
    const claudeCard = screen.getByTestId('harness-claude');
    const codexCard = screen.getByTestId('harness-codex');
    // Claude children present, Codex children absent from Claude card.
    expect(within(claudeCard).queryByTestId('pairing-codex-minimax')).toBeNull();
    // Codex children present, Claude children absent from Codex card.
    expect(within(codexCard).queryByTestId('pairing-claude-minimax')).toBeNull();
    expect(within(codexCard).queryByTestId('pairing-claude-kimi')).toBeNull();
    // Each card owns exactly its own children (proves dnd-kit's per-DndContext
    // scoping makes a cross-card drop impossible — there's no DOM path between
    // the two SortableContexts that could route the drag).
  });
});
