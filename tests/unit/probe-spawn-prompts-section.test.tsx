/**
 * Probe spawn prompt settings (Settings → General → Probe spawn prompts).
 *
 * The section edits the two custom templates for the initial prompt
 * handed to agents spawned from the Probe's GitHub Issues / Pull
 * Requests tabs. Each test pins one observable contract:
 *
 *   - empty stored values render the "Using default" badge with the
 *     built-in template as editable textarea text;
 *   - a stored custom template renders "Custom" plus a Reset button;
 *   - blur saves the draft through `onSave`, success commits it;
 *   - a rejected save rolls the draft back to the committed value;
 *   - Reset clears through `onReset`;
 *   - the section stays disabled until the built-in defaults resolve.
 *
 * Assertions use plain DOM properties (no jest-dom matchers — the repo
 * has no jest-dom setup; see tests/setup/vitest.setup.ts).
 */
import { describe, it, expect, vi } from 'vitest';
import { act, render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ProbeSpawnPromptsSection } from '../../src/components/AppSettings/ProbeSpawnPromptsSection';

const DEFAULTS = {
  issue: 'Please work on GitHub issue #{{number}}{{title_suffix}}\n{{url}}',
  pr: 'Review PR #{{number}}\n{{policy}}\n{{url}}',
  policy: 'Inspect changes, tests and previous findings. Report actionable problems and an explicit verdict.',
};

function renderSection(overrides: {
  stored?: { issue: string | null; pr: string | null };
  defaults?: typeof DEFAULTS | null;
  onSave?: (kind: 'issue' | 'pr', value: string) => Promise<boolean>;
  onReset?: (kind: 'issue' | 'pr') => Promise<boolean>;
  disabled?: boolean;
} = {}) {
  const onSave = overrides.onSave ?? vi.fn(async () => true);
  const onReset = overrides.onReset ?? vi.fn(async () => true);
  const onDirtyChange = vi.fn();
  const view = render(
    <ProbeSpawnPromptsSection
      stored={overrides.stored ?? { issue: null, pr: null }}
      defaults={overrides.defaults === undefined ? DEFAULTS : overrides.defaults}
      onSave={onSave}
      onReset={onReset}
      onDirtyChange={onDirtyChange}
      disabled={overrides.disabled ?? false}
    />,
  );
  return { onSave, onReset, onDirtyChange, ...view };
}

function inputValue(testId: string): string {
  return (screen.getByTestId(testId) as HTMLTextAreaElement).value;
}

describe('ProbeSpawnPromptsSection', () => {
  it('populates editable defaults without saving an override on untouched blur', () => {
    const { onSave } = renderSection();
    expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Using default');
    expect(screen.getByTestId('probe-prompt-badge-pr').textContent).toBe('Using default');
    expect(inputValue('probe-prompt-input-issue')).toBe(DEFAULTS.issue);
    expect(inputValue('probe-prompt-input-pr')).toBe(DEFAULTS.pr);
    fireEvent.blur(screen.getByTestId('probe-prompt-input-issue'));
    fireEvent.blur(screen.getByTestId('probe-prompt-input-pr'));
    expect(onSave).not.toHaveBeenCalled();
    // No reset affordance when nothing custom is stored.
    expect(screen.queryByTestId('probe-prompt-reset-issue')).toBeNull();
    expect(screen.queryByTestId('probe-prompt-reset-pr')).toBeNull();
  });

  it('renders a stored custom template with the Custom badge and a Reset button', () => {
    renderSection({ stored: { issue: 'Custom #{{number}}', pr: null } });
    expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Custom');
    expect(inputValue('probe-prompt-input-issue')).toBe('Custom #{{number}}');
    expect(screen.getByTestId('probe-prompt-reset-issue')).not.toBeNull();
    // The untouched side stays on the default.
    expect(screen.getByTestId('probe-prompt-badge-pr').textContent).toBe('Using default');
  });

  it('saves the draft on blur and commits the Custom badge on success', async () => {
    const onSave = vi.fn(async (_kind: 'issue' | 'pr', _value: string) => true);
    const { onDirtyChange } = renderSection({ onSave });
    const input = screen.getByTestId('probe-prompt-input-pr');
    fireEvent.change(input, { target: { value: 'Look at {{url}}' } });
    expect(screen.queryByTestId('probe-prompt-dirty-pr')).not.toBeNull();
    // The edit marks the modal dirty so the discard banner can intercept.
    await waitFor(() => expect(onDirtyChange).toHaveBeenCalledWith(true));
    fireEvent.blur(input);
    await waitFor(() => expect(onSave).toHaveBeenCalledWith('pr', 'Look at {{url}}'));
    await waitFor(() =>
      expect(screen.getByTestId('probe-prompt-badge-pr').textContent).toBe('Custom'),
    );
    // The committed save clears the modal dirty signal again.
    await waitFor(() => expect(onDirtyChange).toHaveBeenCalledWith(false));
  });

  it('rolls the draft back to the committed value when the save fails', async () => {
    const onSave = vi.fn(async (_kind: 'issue' | 'pr', _value: string) => false);
    const { onDirtyChange } = renderSection({
      stored: { issue: 'Kept #{{number}}', pr: null },
      onSave,
    });
    const input = screen.getByTestId('probe-prompt-input-issue');
    fireEvent.change(input, { target: { value: 'Discarded edit' } });
    fireEvent.blur(input);
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    await waitFor(() => expect(inputValue('probe-prompt-input-issue')).toBe('Kept #{{number}}'));
    // Rollback restores the committed value, so the dirty signal clears.
    await waitFor(() => expect(onDirtyChange).toHaveBeenCalledWith(false));
  });

  it('clears the override through onReset', async () => {
    const onReset = vi.fn(async (_kind: 'issue' | 'pr') => true);
    const { onDirtyChange } = renderSection({
      stored: { issue: 'Custom #{{number}}', pr: null },
      onReset,
    });
    // Touch the draft first so the reset observably clears a dirty card.
    fireEvent.change(screen.getByTestId('probe-prompt-input-issue'), {
      target: { value: 'Edited #{{number}}' },
    });
    await waitFor(() => expect(onDirtyChange).toHaveBeenCalledWith(true));
    fireEvent.click(screen.getByTestId('probe-prompt-reset-issue'));
    await waitFor(() => expect(onReset).toHaveBeenCalledWith('issue'));
    await waitFor(() =>
      expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Using default'),
    );
    await waitFor(() => expect(onDirtyChange).toHaveBeenCalledWith(false));
    expect(inputValue('probe-prompt-input-issue')).toBe(DEFAULTS.issue);
  });

  it('loads defaults into untouched fields and preserves a custom prompt during hydration', () => {
    const onSave = vi.fn(async () => true);
    const onReset = vi.fn(async () => true);
    const { rerender } = renderSection({
      stored: { issue: 'Saved issue prompt', pr: null }, defaults: null, onSave, onReset,
    });
    rerender(<ProbeSpawnPromptsSection
      stored={{ issue: 'Saved issue prompt', pr: null }} defaults={DEFAULTS}
      onSave={onSave} onReset={onReset}
    />);
    expect(inputValue('probe-prompt-input-issue')).toBe('Saved issue prompt');
    expect(inputValue('probe-prompt-input-pr')).toBe(DEFAULTS.pr);
    expect(screen.queryByTestId('probe-prompt-dirty-pr')).toBeNull();
    expect(onSave).not.toHaveBeenCalled();
  });

  it('restores editable default wording when a cleared prompt is saved', async () => {
    const { onSave } = renderSection({ stored: { issue: 'Custom', pr: null } });
    const input = screen.getByTestId('probe-prompt-input-issue');
    fireEvent.change(input, { target: { value: '   ' } });
    fireEvent.blur(input);
    await waitFor(() => expect(onSave).toHaveBeenCalledWith('issue', ''));
    await waitFor(() => expect(inputValue('probe-prompt-input-issue')).toBe(DEFAULTS.issue));
    expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Using default');
  });

  it('restores the default after a failed edit of the built-in wording', async () => {
    renderSection({ onSave: vi.fn(async () => false) });
    const input = screen.getByTestId('probe-prompt-input-pr');
    fireEvent.change(input, { target: { value: 'Rejected edit' } });
    fireEvent.blur(input);
    await waitFor(() => expect(inputValue('probe-prompt-input-pr')).toBe(DEFAULTS.pr));
    expect(screen.getByTestId('probe-prompt-badge-pr').textContent).toBe('Using default');
  });

  it('preserves typing during a pending reset and saves it against the restored default', async () => {
    let resolveReset!: (ok: boolean) => void;
    const onReset = vi.fn(() => new Promise<boolean>((resolve) => { resolveReset = resolve; }));
    const { onSave } = renderSection({ stored: { issue: 'Custom', pr: null }, onReset });
    fireEvent.click(screen.getByTestId('probe-prompt-reset-issue'));
    const input = screen.getByTestId('probe-prompt-input-issue');
    fireEvent.change(input, { target: { value: 'Newer edit' } });
    await act(async () => resolveReset(true));
    expect(inputValue('probe-prompt-input-issue')).toBe('Newer edit');
    expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Using default');
    expect(screen.queryByTestId('probe-prompt-dirty-issue')).not.toBeNull();
    fireEvent.blur(input);
    await waitFor(() => expect(onSave).toHaveBeenCalledWith('issue', 'Newer edit'));
  });

  it('ignores an older reset completion after a newer edit has saved', async () => {
    let resolveReset!: (ok: boolean) => void;
    const onReset = vi.fn(() => new Promise<boolean>((resolve) => { resolveReset = resolve; }));
    renderSection({ stored: { issue: 'Custom', pr: null }, onReset });
    fireEvent.click(screen.getByTestId('probe-prompt-reset-issue'));
    const input = screen.getByTestId('probe-prompt-input-issue');
    fireEvent.change(input, { target: { value: 'New saved edit' } });
    fireEvent.blur(input);
    await waitFor(() => expect(screen.queryByTestId('probe-prompt-dirty-issue')).toBeNull());
    await act(async () => resolveReset(true));
    expect(inputValue('probe-prompt-input-issue')).toBe('New saved edit');
    expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Custom');
    expect(screen.queryByTestId('probe-prompt-dirty-issue')).toBeNull();
  });

  it('ignores an older save completion after a newer reset', async () => {
    let resolveSave!: (ok: boolean) => void;
    const onSave = vi.fn(() => new Promise<boolean>((resolve) => { resolveSave = resolve; }));
    renderSection({ stored: { issue: 'Custom', pr: null }, onSave });
    const input = screen.getByTestId('probe-prompt-input-issue');
    fireEvent.change(input, { target: { value: 'Stale saved edit' } });
    fireEvent.blur(input);
    fireEvent.click(screen.getByTestId('probe-prompt-reset-issue'));
    await waitFor(() => expect(inputValue('probe-prompt-input-issue')).toBe(DEFAULTS.issue));
    await act(async () => resolveSave(true));
    expect(inputValue('probe-prompt-input-issue')).toBe(DEFAULTS.issue);
    expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Using default');
    expect(screen.queryByTestId('probe-prompt-dirty-issue')).toBeNull();
  });

  it('resets an edited custom prompt by mouse without saving the discarded draft', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSection({ stored: { issue: 'Custom', pr: null } });
    const input = screen.getByTestId('probe-prompt-input-issue');
    await user.click(input);
    await user.keyboard(' edit');
    await user.click(screen.getByTestId('probe-prompt-reset-issue'));
    await waitFor(() => expect(inputValue('probe-prompt-input-issue')).toBe(DEFAULTS.issue));
    expect(onSave).not.toHaveBeenCalled();
  });

  it('inserts at the cursor, returns focus and saves the full edited default on blur', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSection();
    const input = screen.getByTestId('probe-prompt-input-issue') as HTMLTextAreaElement;
    await user.click(input);
    input.setSelectionRange(7, 7);
    await user.click(screen.getByRole('button', { name: 'Insert {{repo}} into GitHub Issues probe prompt' }));
    const expected = 'Please {{repo}}work on GitHub issue #{{number}}{{title_suffix}}\n{{url}}';
    expect(input.value).toBe(expected);
    expect(input.selectionStart).toBe(15);
    expect(input.selectionEnd).toBe(15);
    expect(document.activeElement).toBe(input);
    expect(onSave).not.toHaveBeenCalled();
    await user.click(screen.getByTestId('probe-prompt-input-pr'));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith('issue', expected));
  });

  it('replaces the selection and supports keyboard activation without an intermediate save', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSection({ stored: { issue: 'Keep selected text', pr: null } });
    const input = screen.getByTestId('probe-prompt-input-issue') as HTMLTextAreaElement;
    await user.click(input);
    input.setSelectionRange(5, 13);
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Insert {{number}} into GitHub Issues probe prompt' }));
    expect(onSave).not.toHaveBeenCalled();
    await user.keyboard('{Enter}');
    expect(input.value).toBe('Keep {{number}} text');
    expect(input.selectionStart).toBe(15);
    expect(document.activeElement).toBe(input);
    expect(onSave).not.toHaveBeenCalled();
  });

  it('updates example prompts with only supported tokens and the backend review policy', () => {
    renderSection();
    expect(screen.getByTestId('probe-prompt-preview-issue').textContent).toBe(
      'Please work on GitHub issue #42 — Fix login timeout\nhttps://github.com/octocat/hello-world/issues/42',
    );
    expect(screen.getByTestId('probe-prompt-preview-pr').textContent).toBe(
      `Review PR #42\n${DEFAULTS.policy}\nhttps://github.com/octocat/hello-world/pull/42`,
    );
    fireEvent.change(screen.getByTestId('probe-prompt-input-issue'), {
      target: { value: '{{owner}}/{{repo}} {{title}} {{policy}} {{unknown}} {{toString}} {{url' },
    });
    expect(screen.getByTestId('probe-prompt-preview-issue').textContent).toBe(
      'octocat/hello-world Fix login timeout {{policy}} {{unknown}} {{toString}} {{url',
    );
    fireEvent.change(screen.getByTestId('probe-prompt-input-pr'), {
      target: { value: '{{title}} {{policy}}' },
    });
    expect(screen.getByTestId('probe-prompt-preview-pr').textContent).toBe(`{{title}} ${DEFAULTS.policy}`);
  });

  it('does not expand placeholder-shaped text inside the review policy', () => {
    renderSection({ defaults: { ...DEFAULTS, policy: 'Review {{repo}} literally.' } });
    expect(screen.getByTestId('probe-prompt-preview-pr').textContent).toBe(
      'Review PR #42\nReview {{repo}} literally.\nhttps://github.com/octocat/hello-world/pull/42',
    );
  });

  it('disables placeholder insertion when settings cannot be edited', () => {
    renderSection({ disabled: true });
    expect(screen.getByRole('button', { name: 'Insert {{policy}} into Pull Requests probe prompt' }).hasAttribute('disabled')).toBe(true);
  });

  it('trims padding at the commit boundary so the banner clears', async () => {
    const onSave = vi.fn(async (_kind: 'issue' | 'pr', _value: string) => true);
    renderSection({ onSave });
    const input = screen.getByTestId('probe-prompt-input-issue');
    fireEvent.change(input, { target: { value: '  padded {{number}}  ' } });
    fireEvent.blur(input);
    // The backend stores the trimmed value, so the draft commits trimmed.
    await waitFor(() => expect(onSave).toHaveBeenCalledWith('issue', 'padded {{number}}'));
    await waitFor(() => expect(inputValue('probe-prompt-input-issue')).toBe('padded {{number}}'));
    await waitFor(() => expect(screen.queryByTestId('probe-prompt-dirty-issue')).toBeNull());
  });

  it('keeps keystrokes typed while a save is in flight', async () => {
    let resolveSave!: (ok: boolean) => void;
    const onSave = vi.fn(
      (_kind: 'issue' | 'pr', _value: string) =>
        new Promise<boolean>((resolve) => {
          resolveSave = resolve;
        }),
    );
    renderSection({ onSave });
    const input = screen.getByTestId('probe-prompt-input-issue');
    fireEvent.change(input, { target: { value: 'first' } });
    fireEvent.blur(input);
    await waitFor(() => expect(onSave).toHaveBeenCalledWith('issue', 'first'));
    // Type again before the save resolves: the settle must not clobber it.
    fireEvent.change(input, { target: { value: 'second' } });
    resolveSave(true);
    await waitFor(() => expect(inputValue('probe-prompt-input-issue')).toBe('second'));
    // The newer edit is still unsaved against the stored baseline.
    expect(screen.queryByTestId('probe-prompt-dirty-issue')).not.toBeNull();
  });

  it('disables both inputs until the built-in defaults resolve', () => {
    renderSection({ defaults: null });
    expect(screen.getByTestId('probe-prompt-input-issue').hasAttribute('disabled')).toBe(true);
    expect(screen.getByTestId('probe-prompt-input-pr').hasAttribute('disabled')).toBe(true);
  });

  it('surfaces a defaults load failure with a working Retry button', () => {
    const onRetryDefaults = vi.fn();
    render(
      <ProbeSpawnPromptsSection
        stored={{ issue: null, pr: null }}
        defaults={null}
        onSave={vi.fn(async () => true)}
        onReset={vi.fn(async () => true)}
        onDirtyChange={vi.fn()}
        defaultsError="ipc down"
        onRetryDefaults={onRetryDefaults}
      />,
    );
    expect(screen.getByTestId('probe-prompts-defaults-error').textContent).toContain('ipc down');
    fireEvent.click(screen.getByTestId('probe-prompts-defaults-retry'));
    expect(onRetryDefaults).toHaveBeenCalledTimes(1);
  });
});
