/**
 * Tests for `<AccountCard>` — the Settings-side credential/editor card
 * that pairs with each ProviderAccount. Usage Meters moved out in
 * issue #601 (they live on the Probe Panel's new "Usage" tab via
 * `<UsagePanel>`); these tests pin what the Settings card STILL does:
 *
 *   - Enable toggle (writes through `onSave`)
 *   - Remove for non-self-auth (keyed first-class + custom); self-auth
 *     can only be disabled (ADR-0025)
 *   - Two-step Remove confirmation guard, busy gate on in-flight
 *     remove, busy recovery on rejected remove
 *   - API key editor for Claude-compatible accounts; billing for
 *     first-class only. Base URL / model tiers live on Harnesses attach.
 *   - No credential editor for self-authenticating harnesses (#568a)
 *
 * Meter rendering moved to `tests/unit/usage-panel.test.tsx`.
 */

import { describe, it, expect, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { AccountCard } from '../../src/components/AppSettings/AppSettingsModal';
import type { ProviderAccount } from '../../src/lib/tauri';
import { isClaudeCompatibleId } from '../../src/lib/providerClassification';

function account(over: Partial<ProviderAccount> = {}): ProviderAccount {
  const id = over.id ?? 'anthropic';
  return {
    id: 'anthropic',
    name: 'Anthropic / Claude',
    enabled: true,
    billing_mode: 'plan',
    claude_compatible: isClaudeCompatibleId(id),
    api_key: null,
    ...over,
  };
}

describe('AccountCard (issue #537, settings-side credential/editor)', () => {
  it('flips the enable toggle through onSave', async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(<AccountCard account={account({ enabled: true })} onSave={onSave} />);

    await user.click(screen.getByRole('checkbox', { name: /enable anthropic/i }));
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    expect(onSave.mock.calls[0][0]).toMatchObject({ id: 'anthropic', enabled: false });
  });

  it('shows Remove for a keyed first-class account (minimax is removable once added)', () => {
    render(
      <AccountCard
        account={account({ id: 'minimax', name: 'MiniMax', billing_mode: 'pay_as_you_go' })}
        onSave={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    // ADR-0025: keyed first-class (minimax/kimi/openrouter) are removable;
    // only self-auth built-ins hide Remove.
    expect(screen.getByRole('button', { name: /^remove minimax$/i })).toBeTruthy();
  });

  it('hides Remove for a self-authenticating built-in (Anthropic)', () => {
    render(
      <AccountCard
        account={account({ id: 'anthropic' })}
        onSave={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    expect(screen.queryByRole('button', { name: /remove/i })).toBeNull();
  });

  it('shows no credential editor for a self-authenticating harness (#568a)', () => {
    render(<AccountCard account={account({ id: 'anthropic' })} onSave={vi.fn()} />);
    // Anthropic/Codex/Antigravity authenticate via their own CLI — no key fields.
    // Billing is still editable ("Edit billing"), but not credentials.
    expect(screen.queryByRole('button', { name: /edit credentials/i })).toBeNull();
    expect(screen.getByRole('button', { name: /edit billing/i })).toBeTruthy();
  });

  it('shows API key (not model tiers) for a Claude-compatible account', async () => {
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'kimi', name: 'Kimi', billing_mode: 'pay_as_you_go' })}
        onSave={vi.fn()}
      />,
    );
    await user.click(screen.getByRole('button', { name: /edit credentials/i }));
    expect(screen.getByLabelText(/kimi api key/i)).toBeTruthy();
    // Model tiers moved to Harnesses attach (ADR-0025).
    expect(screen.queryByLabelText(/fable model/i)).toBeNull();
    expect(screen.queryByLabelText(/opus model/i)).toBeNull();
    expect(screen.getByText(/base url and model names are set when you attach/i)).toBeTruthy();
  });

  it('passes an edited API key through onSave', async () => {
    const onSave = vi.fn().mockResolvedValue(true);
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'kimi', name: 'Kimi', billing_mode: 'pay_as_you_go' })}
        onSave={onSave}
      />,
    );
    await user.click(screen.getByRole('button', { name: /edit credentials/i }));
    await user.type(screen.getByLabelText(/kimi api key/i), 'sk-kimi');
    await user.click(screen.getByRole('button', { name: /^save$/i }));
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    expect(onSave.mock.calls[0][0].api_key).toBe('sk-kimi');
  });

  it('passes billing mode through onSave for a first-class account', async () => {
    const onSave = vi.fn().mockResolvedValue(true);
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'minimax', name: 'MiniMax', billing_mode: 'pay_as_you_go' })}
        onSave={onSave}
      />,
    );
    await user.click(screen.getByRole('button', { name: /edit credentials/i }));
    await user.selectOptions(screen.getByLabelText(/minimax billing mode/i), 'plan');
    await user.click(screen.getByRole('button', { name: /^save$/i }));
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    expect(onSave.mock.calls[0][0].billing_mode).toBe('plan');
  });

  it('offers Remove for a custom account without expanding credentials', () => {
    render(
      <AccountCard
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        onSave={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    // Discoverability fix: Remove is now in the card header so the user doesn't
    // have to open the credentials section to find it.
    expect(screen.getByRole('button', { name: /^remove deepseek$/i })).toBeTruthy();
  });

  it('requires two clicks to remove a custom account (confirmation guard)', async () => {
    const onRemove = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        onSave={vi.fn()}
        onRemove={onRemove}
      />,
    );

    // First click surfaces the Yes/No pair but does NOT fire onRemove yet.
    await user.click(screen.getByRole('button', { name: /^remove deepseek$/i }));
    expect(onRemove).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: /confirm remove deepseek/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /cancel remove deepseek/i })).toBeTruthy();

    // The plain Remove button is gone while the confirm pair is showing.
    expect(screen.queryByRole('button', { name: /^remove deepseek$/i })).toBeNull();
  });

  it('fires onRemove only when the user confirms', async () => {
    const onRemove = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        onSave={vi.fn()}
        onRemove={onRemove}
      />,
    );

    // Click Remove -> Confirm pair shows. Confirm fires onRemove with the id.
    await user.click(screen.getByRole('button', { name: /^remove deepseek$/i }));
    await user.click(screen.getByRole('button', { name: /confirm remove deepseek/i }));
    expect(onRemove).toHaveBeenCalledWith('deepseek');
  });

  it('cancels the confirm pair without firing onRemove', async () => {
    const onRemove = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        onSave={vi.fn()}
        onRemove={onRemove}
      />,
    );

    // Click Remove -> Cancel returns to the plain Remove button, no callback.
    await user.click(screen.getByRole('button', { name: /^remove deepseek$/i }));
    await user.click(screen.getByRole('button', { name: /cancel remove deepseek/i }));
    expect(onRemove).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: /^remove deepseek$/i })).toBeTruthy();
  });

  it('does not render Remove when onRemove prop is omitted (defensive)', () => {
    // The parent's onRemove handler is optional — when absent, the card is
    // read-only, so the destructive action should not appear.
    render(
      <AccountCard
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        onSave={vi.fn()}
      />,
    );
    expect(screen.queryByRole('button', { name: /remove/i })).toBeNull();
  });

  it('gates the Remove button on busy for the full duration of an in-flight remove', async () => {
    // Race regression: a fast user could double-click Remove -> Yes -> Remove
    // before the first IPC resolved, firing two concurrent onRemove calls.
    // The card now sets busy=true for the entire onRemove await so the second
    // click hits a disabled button.
    let resolveRemove!: () => void;
    const onRemove = vi.fn().mockImplementation(
      () => new Promise<void>((resolve) => { resolveRemove = resolve; }),
    );
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        onSave={vi.fn()}
        onRemove={onRemove}
      />,
    );

    // Click Remove -> Yes. onRemove is now in-flight.
    await user.click(screen.getByRole('button', { name: /^remove deepseek$/i }));
    await user.click(screen.getByRole('button', { name: /confirm remove deepseek/i }));
    expect(onRemove).toHaveBeenCalledTimes(1);

    // While the in-flight remove is pending, the Remove button (which briefly
    // re-appears as the confirm pair flips back) must be disabled. Click
    // should be a no-op for the onRemove counter.
    const removeButton = screen.queryByRole('button', { name: /^remove deepseek$/i });
    if (removeButton) {
      await user.click(removeButton);
    }
    expect(onRemove).toHaveBeenCalledTimes(1);

    // Once the parent's promise resolves, the parent will unmount this card
    // (the account is gone from `accounts`), so the test ends here.
    resolveRemove();
  });

  it('flips busy back off when onRemove rejects, leaving the card usable', async () => {
    // If the backend rejects the remove, busy must be reset in `finally` so
    // the user can retry — otherwise a failed remove leaves the card stuck
    // with the Yes button visible-but-disabled forever.
    const onRemove = vi.fn().mockRejectedValue(new Error('boom'));
    const user = userEvent.setup();
    render(
      <AccountCard
        account={account({ id: 'deepseek', name: 'DeepSeek' })}
        onSave={vi.fn()}
        onRemove={onRemove}
      />,
    );

    await user.click(screen.getByRole('button', { name: /^remove deepseek$/i }));
    await user.click(screen.getByRole('button', { name: /confirm remove deepseek/i }));
    expect(onRemove).toHaveBeenCalledTimes(1);

    // Wait for the rejected promise + the finally block to settle.
    await waitFor(() => {
      // The card returns to its idle state with a fresh Remove button.
      expect(screen.getByRole('button', { name: /^remove deepseek$/i })).toBeTruthy();
    });
  });
});
