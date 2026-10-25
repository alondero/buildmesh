import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { buildInitScript } from '../../scripts/ui-mock/tauri-mock.mjs';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { ErrorBoundary } from '../../src/components/ErrorBoundary/ErrorBoundary';
import { openSettingsPane } from '../utils/settings-panes';

// Exercise the browser mock through the real IPC facet wrappers.
vi.unmock('@tauri-apps/api/core');

afterEach(() => vi.restoreAllMocks());

describe('default mock Settings contracts', () => {
  it('supplies configurable provider capabilities through IPC', async () => {
    eval(buildInitScript());
    const providers = await window.__TAURI_INTERNALS__.invoke('list_providers');
    expect(providers[0]).toMatchObject({
      label: 'Claude', harness_id: 'anthropic',
      capabilities: { supports_model_override: true, effort_control: { kind: 'closed' } },
    });
  });

  it('supplies nonempty prompt templates through IPC', async () => {
    eval(buildInitScript());
    const templates = await window.__TAURI_INTERNALS__.invoke('get_probe_spawn_prompt_defaults');
    expect(templates).toEqual({
      issue_template: expect.stringContaining('{{url}}'),
      pr_template: expect.stringContaining('{{policy}}'),
      review_policy: expect.stringMatching(/\S/),
    });
  });

  it('renders Settings harness controls and enabled prompt editors with default IPC', async () => {
    eval(buildInitScript());
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    console.error('collector control');
    expect(errors).toHaveBeenCalledWith('collector control');
    errors.mockClear();

    render(<ErrorBoundary><AppSettingsModal onClose={() => {}} /></ErrorBoundary>);
    await openSettingsPane('General');
    expect(await screen.findByRole('checkbox', { name: 'Confirm before quitting' })).toHaveProperty('checked', true);
    await openSettingsPane('Harnesses');
    await waitFor(() => expect(screen.queryByRole('textbox', { name: 'Claude default model' })).toBeTruthy());
    await openSettingsPane('Prompts');
    await waitFor(() => {
      const issue = screen.getByTestId('probe-prompt-input-issue') as HTMLTextAreaElement;
      expect(issue.disabled).toBe(false);
      expect(issue.value).toContain('{{url}}');
      const pr = screen.getByTestId('probe-prompt-input-pr') as HTMLTextAreaElement;
      expect(pr.disabled).toBe(false);
      expect(pr.value).toContain('{{policy}}');
    });
    expect(errors.mock.calls).toEqual([]);
  });
});
