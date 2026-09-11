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

vi.mock('../../src/lib/tauri', async () => {
  const actual = await vi.importActual<typeof import('../../src/lib/tauri')>('../../src/lib/tauri');
  return {
    ...actual,
  };
});

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

  it('uses Muse account quota without a manual plan or API key editor', () => {
    render(<AccountCard account={account({ id: 'muse-code', name: 'Meta Muse Code' })} onSave={vi.fn()} />);
    expect(screen.queryByRole('button', { name: /edit credentials/i })).toBeNull();
    expect(screen.queryByRole('combobox', { name: /muse code subscription plan/i })).toBeNull();
    expect(screen.queryByText(/counts requests locally/i)).toBeNull();
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

  // -----------------------------------------------------------------
  // Issue #1535 (PR #1636 review round 3): per-field overrides, no
  // field shadowing, unmount cleanup, `''` vs `null` normalisation.
  // These five cases pin the architectural shape so future refactors
  // can't reintroduce the round-3 sync-effect / shared-shadow-ref
  // patterns.
  // -----------------------------------------------------------------
  describe('per-field editable delta (PR #1636 review round 3)', () => {
    function keyedAccount(over: Partial<ProviderAccount> = {}): ProviderAccount {
      return account({
        id: 'kimi',
        name: 'Kimi',
        billing_mode: 'pay_as_you_go',
        claude_compatible: true,
        api_key: null,
        ...over,
      });
    }

    it('unmount clears the modal\'s dirty site (no leaked banner after Remove)', async () => {
      // Finding 1: the JSDoc on `onDirtyChange` promises `false` on
      // unmount. Without the cleanup, removing a dirty card (Remove
      // button) leaves the modal's `dirtySites` set with a stale entry,
      // permanently trapping the user with the discard banner.
      const onDirtyChange = vi.fn();
      const user = userEvent.setup();
      const { unmount } = render(
        <AccountCard
          account={keyedAccount()}
          onSave={vi.fn().mockResolvedValue(true)}
          onDirtyChange={onDirtyChange}
        />,
      );

      // User types an api_key → dirty = true fires once.
      await user.click(screen.getByRole('button', { name: /edit credentials/i }));
      await user.type(screen.getByLabelText(/kimi api key/i), 'sk-x');
      expect(onDirtyChange).toHaveBeenLastCalledWith(true);

      // Unmounting the dirty card must fire false to clear the modal's
      // dirty site.
      unmount();
      expect(onDirtyChange).toHaveBeenLastCalledWith(false);
    });

    it('editing one field does NOT capture the other (server-side rename of the untouched field rides through on save)', async () => {
      // Finding 2: a monolithic draft snapshot freezes every editable
      // field on first edit, so a concurrent server-side change to a
      // field the user DIDN'T touch gets clobbered on save. Per-field
      // overrides fix this: editing only api_key leaves billing_mode
      // un-overridden, so save picks up `account.billing_mode` directly.
      const onSave = vi.fn().mockResolvedValue(true);
      const user = userEvent.setup();
      const { rerender } = render(
        <AccountCard
          account={keyedAccount({ billing_mode: 'plan' })}
          onSave={onSave}
        />,
      );

      // Server-side change to billing_mode while the user is editing api_key.
      rerender(
        <AccountCard
          account={keyedAccount({ billing_mode: 'pay_as_you_go' })}
          onSave={onSave}
        />,
      );

      // User edits only api_key.
      await user.click(screen.getByRole('button', { name: /edit credentials/i }));
      await user.type(screen.getByLabelText(/kimi api key/i), 'sk-new');
      await user.click(screen.getByRole('button', { name: /^save$/i }));

      await waitFor(() => expect(onSave).toHaveBeenCalled());
      const payload = onSave.mock.calls[0][0];
      // The user's typed key is committed...
      expect(payload.api_key).toBe('sk-new');
      // ...but billing_mode is NOT a frozen snapshot from when the user
      // started typing — it's the LATEST server value, which is what
      // would be sent if the user hadn't edited that field at all.
      expect(payload.billing_mode).toBe('pay_as_you_go');
    });

    it('an empty-string api_key baseline normalises to null (smart collapse actually fires)', async () => {
      // Finding 3: `?? null` lets an empty-string baseline leak through,
      // trapping the card dirty after a backspace. Normalising on the
      // way in (`|| null`) means backspace-to-empty and server-null
      // both collapse the same way.
      const user = userEvent.setup();
      // Build the fixture manually so the api_key really is '' on the
      // baseline. AccountCard's type allows `string | null | undefined`,
      // so we cast to satisfy the helper while exercising the
      // normalisation path.
      const emptyKey = account({
        id: 'kimi',
        name: 'Kimi',
        billing_mode: 'pay_as_you_go',
        claude_compatible: true,
      }) as ProviderAccount;
      Object.assign(emptyKey, { api_key: '' });
      render(<AccountCard account={emptyKey} onSave={vi.fn().mockResolvedValue(true)} />);
      await user.click(screen.getByRole('button', { name: /edit credentials/i }));

      // The baseline '' is normalised to null internally, so the input
      // starts empty (not stuck on the empty-string from the prop).
      const input = screen.getByLabelText(/kimi api key/i) as HTMLInputElement;
      expect(input.value).toBe('');
    });

    it('isDirty is derived purely from the override set (no manual flag)', async () => {
      // Finding 4: `Object.keys(overrides).length > 0` is the
      // derivation. We don't have access to internal state, but we can
      // observe it indirectly via the dirty-callback — if isDirty were
      // maintained imperatively in the setters, this test would catch a
      // regression where adding then removing an override leaves the
      // card "dirty" forever.
      const onDirtyChange = vi.fn();
      const user = userEvent.setup();
      render(
        <AccountCard
          account={keyedAccount()}
          onSave={vi.fn().mockResolvedValue(true)}
          onDirtyChange={onDirtyChange}
        />,
      );

      await user.click(screen.getByRole('button', { name: /edit credentials/i }));

      // Type and revert — each keystroke after the first is a non-baseline
      // value, but the FINAL state (empty input) IS the baseline, so the
      // smart collapse should leave the card pristine.
      await user.type(screen.getByLabelText(/kimi api key/i), 'sk-x');
      expect(onDirtyChange).toHaveBeenLastCalledWith(true);

      await user.clear(screen.getByLabelText(/kimi api key/i));
      expect(onDirtyChange).toHaveBeenLastCalledWith(false);
    });
  });

  // -----------------------------------------------------------------
  // Issue #1535: preserve dirty provider credentials across the
  // prop-synchronisation race the parent causes. The card must keep
  // a typed API key when (a) the user toggles Enabled while editing,
  // (b) a save IPC fails and the parent rolls the prop back, and
  // (c) an unrelated prop refresh fires while the draft is dirty.
  // PR #1636 review fixes: non-atomic toggle policy, per-field
  // overrides (no field shadowing), save composes from the latest
  // prop, mock `return false` (production path), defensive catch for
  // throws.
  // -----------------------------------------------------------------
  describe('dirty credentials survive prop churn (issue #1535)', () => {
    function claudeCompatibleAccount(over: Partial<ProviderAccount> = {}): ProviderAccount {
      return account({
        id: 'kimi',
        name: 'Kimi',
        billing_mode: 'pay_as_you_go',
        claude_compatible: true,
        api_key: null,
        ...over,
      });
    }

    it('toggle commits ONLY enabled; typed draft survives the parent re-render after the IPC', async () => {
      // Non-atomic toggle policy: the toggle's IPC payload must NOT include
      // any typed-but-unsaved api_key. The user's draft is preserved through
      // the parent's post-toggle re-render (handleSaveAccount's optimistic
      // `setAccounts(prev.map(...))` + re-fetch) so explicit Save commits
      // it. This is Finding 1 + Finding 2: round-2 reset userTouchedRef on
      // toggle success, so the prop-sync effect then wiped the draft; the
      // test only caught this once the rerender step was added.
      const onSave = vi.fn().mockResolvedValue(true);
      const user = userEvent.setup();
      const { rerender } = render(
        <AccountCard
          account={claudeCompatibleAccount({ enabled: true })}
          onSave={onSave}
        />,
      );

      await user.click(screen.getByRole('button', { name: /edit credentials/i }));
      await user.type(screen.getByLabelText(/kimi api key/i), 'sk-typed');
      // Toggle Enabled off — must NOT commit the typed api_key.
      await user.click(screen.getByRole('checkbox', { name: /enable kimi/i }));

      await waitFor(() => expect(onSave).toHaveBeenCalled());
      const payload = onSave.mock.calls[0][0];
      // The toggle commits enabled only. The typed api_key is NOT in the
      // IPC payload — it stays in the draft for explicit Save. The payload
      // carries the account's pre-existing api_key (null in this fixture),
      // not the user's typed value.
      expect(payload).toMatchObject({ id: 'kimi', enabled: false });
      expect(payload.api_key).not.toBe('sk-typed');

      // Simulate the parent's optimistic update + re-fetch after the IPC:
      // account arrives with the post-toggle enabled=false. The typed
      // api_key MUST survive this re-render. (Round-2's userTouchedRef was
      // reset by persist's success branch, so this rerender would wipe the
      // draft — the original test missed this because it never rerendered.)
      rerender(
        <AccountCard
          account={claudeCompatibleAccount({ enabled: false })}
          onSave={onSave}
        />,
      );
      expect((screen.getByLabelText(/kimi api key/i) as HTMLInputElement).value).toBe('sk-typed');
    });

    it('rejected save (return false, production path) preserves typed key, dirty state, and inline retry', async () => {
      // Production's handleSaveAccount catches IPC throws and returns
      // `false` (the inline alert shows `'Save failed'`, and the parent
      // toast carries the underlying message). We pin THAT path — a
      // throw-only mock would be a paper-tiger test that wouldn't survive
      // a future adapter change.
      const onSave = vi
        .fn()
        .mockResolvedValueOnce(false)
        .mockResolvedValueOnce(true);
      const user = userEvent.setup();
      render(
        <AccountCard
          account={claudeCompatibleAccount()}
          onSave={onSave}
        />,
      );

      await user.click(screen.getByRole('button', { name: /edit credentials/i }));
      await user.type(screen.getByLabelText(/kimi api key/i), 'sk-precious');

      // First Save returns false.
      await user.click(screen.getByRole('button', { name: /^save$/i }));
      await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));

      // Typed key survives the parent-side rollback.
      expect((screen.getByLabelText(/kimi api key/i) as HTMLInputElement).value).toBe('sk-precious');
      // The credentials editor stays open.
      expect(screen.getByLabelText(/kimi api key/i)).toBeTruthy();
      // Inline error offers Retry.
      const alert = await screen.findByRole('alert');
      expect(alert.textContent).toMatch(/save failed/i);
      const retry = screen.getByRole('button', { name: /^retry$/i });
      expect(retry).toBeTruthy();
      // Retry re-sends the SAME draft.
      await user.click(retry);
      await waitFor(() => expect(onSave).toHaveBeenCalledTimes(2));
      expect(onSave.mock.calls[1][0].api_key).toBe('sk-precious');
    });

    it('rejected save (throw, defensive path) surfaces the backend message inline', async () => {
      // Defensive catch: if a future onSave adapter forgets to wrap its
      // IPC throws and they propagate, the inline alert should still carry
      // the backend's message so the user has a clue what went wrong.
      const onSave = vi.fn().mockRejectedValueOnce(new Error('backend says nope'));
      const user = userEvent.setup();
      render(
        <AccountCard
          account={claudeCompatibleAccount()}
          onSave={onSave}
        />,
      );
      await user.click(screen.getByRole('button', { name: /edit credentials/i }));
      await user.type(screen.getByLabelText(/kimi api key/i), 'sk-precious');
      await user.click(screen.getByRole('button', { name: /^save$/i }));
      const alert = await screen.findByRole('alert');
      expect(alert.textContent).toMatch(/backend says nope/);
    });

    it('save payload includes the LATEST account fields (server-side rename preserved)', async () => {
      // Finding 2 regression: the previous design stored a frozen copy of
      // `account` inside the card's draft, so a server-side rename that
      // happened while the user was typing would be clobbered on save
      // (the IPC sent the stale name). The current design composes
      // `{ ...account, api_key, billing_mode }` so the latest `account.name`
      // rides along.
      const onSave = vi.fn().mockResolvedValue(true);
      const user = userEvent.setup();
      const { rerender } = render(
        <AccountCard
          account={claudeCompatibleAccount({ name: 'Kimi' })}
          onSave={onSave}
        />,
      );

      // Server renames the account while the card is mounted.
      rerender(
        <AccountCard
          account={claudeCompatibleAccount({ name: 'Kimi (renamed)' })}
          onSave={onSave}
        />,
      );
      // Header refreshes from the prop — sanity check.
      expect(screen.getByText('Kimi (renamed)')).toBeTruthy();

      // User types a new api_key (different from server).
      await user.click(screen.getByRole('button', { name: /edit credentials/i }));
      await user.type(screen.getByLabelText(/kimi \(renamed\) api key/i), 'sk-new');
      await user.click(screen.getByRole('button', { name: /^save$/i }));

      await waitFor(() => expect(onSave).toHaveBeenCalled());
      const payload = onSave.mock.calls[0][0];
      // Server-side rename is preserved (latest account.name).
      expect(payload.name).toBe('Kimi (renamed)');
      // User's typed delta is committed.
      expect(payload.api_key).toBe('sk-new');
    });

    it('external prop refresh updates a pristine card but does not overwrite a dirty one', async () => {
      // Pristine case: an external refresh (e.g. another account added)
      // must propagate the new committed value into the draft.
      const { rerender } = render(
        <AccountCard
          account={claudeCompatibleAccount({ api_key: null, enabled: false })}
          onSave={vi.fn().mockResolvedValue(true)}
        />,
      );

      await userEvent.click(screen.getByRole('button', { name: /edit credentials/i }));
      const pristineInput = screen.getByLabelText(/kimi api key/i) as HTMLInputElement;
      expect(pristineInput.value).toBe('');

      // External refresh: backend now reports api_key='sk-from-server'.
      rerender(
        <AccountCard
          account={claudeCompatibleAccount({ api_key: 'sk-from-server', enabled: true })}
          onSave={vi.fn().mockResolvedValue(true)}
        />,
      );
      expect((screen.getByLabelText(/kimi api key/i) as HTMLInputElement).value).toBe('sk-from-server');

      // Dirty case: type something, then external refresh fires. The typed
      // key must NOT be overwritten — the per-field override outlives the
      // prop change (the display reads `overrides.api_key ?? baseline`).
      // Display-only fields (the card's `name`) refresh implicitly because
      // the header reads from the prop.
      const user = userEvent.setup();
      await user.clear(screen.getByLabelText(/kimi api key/i));
      await user.type(screen.getByLabelText(/kimi api key/i), 'sk-half-typed');

      rerender(
        <AccountCard
          account={claudeCompatibleAccount({
            api_key: 'sk-from-server',
            enabled: true,
            // Display-only refresh: a rename on the parent must reach the
            // header even while the user is editing.
            name: 'Kimi (renamed)',
          })}
          onSave={vi.fn().mockResolvedValue(true)}
        />,
      );
      expect(
        (screen.getByLabelText(/kimi \(renamed\) api key/i) as HTMLInputElement).value,
      ).toBe('sk-half-typed');
      expect(screen.getByText('Kimi (renamed)')).toBeTruthy();
    });
  });
});
