/**
 * Probe spawn prompt settings (Settings → General → Probe spawn prompts).
 *
 * The section edits the two custom templates for the initial prompt
 * handed to agents spawned from the Probe's GitHub Issues / Pull
 * Requests tabs. Each test pins one observable contract:
 *
 *   - empty stored values render the "Using default" badge with the
 *     built-in template as the textarea placeholder;
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
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { ProbeSpawnPromptsSection } from '../../src/components/AppSettings/ProbeSpawnPromptsSection';

const DEFAULTS = {
  issue: 'Please work on GitHub issue #{{number}}{{title_suffix}}\n{{url}}',
  pr: 'Review PR #{{number}}\n{{policy}}\n{{url}}',
};

function renderSection(overrides: {
  stored?: { issue: string | null; pr: string | null };
  defaults?: { issue: string; pr: string } | null;
  onSave?: (kind: 'issue' | 'pr', value: string) => Promise<boolean>;
  onReset?: (kind: 'issue' | 'pr') => Promise<boolean>;
  disabled?: boolean;
} = {}) {
  const onSave = overrides.onSave ?? vi.fn(async () => true);
  const onReset = overrides.onReset ?? vi.fn(async () => true);
  const onDirtyChange = vi.fn();
  render(
    <ProbeSpawnPromptsSection
      stored={overrides.stored ?? { issue: null, pr: null }}
      defaults={overrides.defaults === undefined ? DEFAULTS : overrides.defaults}
      onSave={onSave}
      onReset={onReset}
      onDirtyChange={onDirtyChange}
      disabled={overrides.disabled ?? false}
    />,
  );
  return { onSave, onReset, onDirtyChange };
}

function inputValue(testId: string): string {
  return (screen.getByTestId(testId) as HTMLTextAreaElement).value;
}

describe('ProbeSpawnPromptsSection', () => {
  it('shows both cards as Using default with the built-in template as placeholder', () => {
    renderSection();
    expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Using default');
    expect(screen.getByTestId('probe-prompt-badge-pr').textContent).toBe('Using default');
    expect(screen.getByTestId('probe-prompt-input-issue').getAttribute('placeholder')).toBe(
      DEFAULTS.issue,
    );
    expect(screen.getByTestId('probe-prompt-input-pr').getAttribute('placeholder')).toBe(
      DEFAULTS.pr,
    );
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
    renderSection({ onSave });
    const input = screen.getByTestId('probe-prompt-input-pr');
    fireEvent.change(input, { target: { value: 'Look at {{url}}' } });
    expect(screen.queryByTestId('probe-prompt-dirty-pr')).not.toBeNull();
    fireEvent.blur(input);
    await waitFor(() => expect(onSave).toHaveBeenCalledWith('pr', 'Look at {{url}}'));
    await waitFor(() =>
      expect(screen.getByTestId('probe-prompt-badge-pr').textContent).toBe('Custom'),
    );
  });

  it('rolls the draft back to the committed value when the save fails', async () => {
    const onSave = vi.fn(async (_kind: 'issue' | 'pr', _value: string) => false);
    renderSection({
      stored: { issue: 'Kept #{{number}}', pr: null },
      onSave,
    });
    const input = screen.getByTestId('probe-prompt-input-issue');
    fireEvent.change(input, { target: { value: 'Discarded edit' } });
    fireEvent.blur(input);
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    await waitFor(() => expect(inputValue('probe-prompt-input-issue')).toBe('Kept #{{number}}'));
  });

  it('clears the override through onReset', async () => {
    const onReset = vi.fn(async (_kind: 'issue' | 'pr') => true);
    renderSection({
      stored: { issue: 'Custom #{{number}}', pr: null },
      onReset,
    });
    fireEvent.click(screen.getByTestId('probe-prompt-reset-issue'));
    await waitFor(() => expect(onReset).toHaveBeenCalledWith('issue'));
    await waitFor(() =>
      expect(screen.getByTestId('probe-prompt-badge-issue').textContent).toBe('Using default'),
    );
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
