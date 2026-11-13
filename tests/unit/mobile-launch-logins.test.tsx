import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Mesh, Provider } from '../../src/mobile/api';
import { launchChoices, launchFields, loginChoiceId } from '../../src/mobile/spawnMenu';
import { SpawnMenuSheet } from '../../src/mobile/components/SpawnMenuSheet';
import CaptureIdea from '../../src/mobile/screens/CaptureIdea';
import { StartReview } from '../../src/mobile/screens/StartReview';

const native = {
  id: 'codex', label: 'Codex', harness_id: 'codex', group_key: 'codex', is_proxied: false,
  capabilities: { accepts_initial_prompt: true },
  logins: [
    { id: 'default', label: 'Default', is_default: true, signed_in: false },
    { id: 'work', label: 'Work', is_default: false, signed_in: true },
    { id: 'guest', label: 'Guest', is_default: false, signed_in: false },
  ],
} as Provider;
const recipe = (id: string, extra = {}) => ({ ...native, id, logins: undefined, label: id,
  configuration: { id, name: id, spawn_option_id: 'codex', model: null, effort: null, extra_args: null, ...extra },
});
const providers = [native, recipe('launch/free'), recipe('launch/work', { login_id: 'work' }),
  recipe('launch/route', { spawn_option_id: 'codex:proxy', provider_route_id: 'proxy' })];
const response = (body: unknown) => new Response(JSON.stringify(body));
afterEach(() => { vi.unstubAllGlobals(); localStorage.clear(); });

describe('phone login choices', () => {
  it('defaults a fresh Capture visit to the first available launch and retains an explicit unavailable choice', () => {
    const unavailable = { ...native, id: 'missing', harness_id: 'missing', group_key: 'missing', logins: undefined, unavailable_reason: 'Executable missing' };
    const props = { meshes: [{ id: 3, name: 'Repo' } as Mesh], providers: [unavailable, native], onStarted: vi.fn(), onAuthFailed: vi.fn() };
    const view = render(<CaptureIdea {...props} />);
    expect((screen.getByLabelText('Agent') as HTMLSelectElement).value).toBe(loginChoiceId('codex', 'default'));
    fireEvent.change(screen.getByLabelText('Your idea'), { target: { value: 'Review this' } });
    expect((screen.getByRole('button', { name: 'Start working on this' }) as HTMLButtonElement).disabled).toBe(false);
    view.unmount();
    render(<CaptureIdea {...props} preset={{ meshId: 3, launchId: 'missing' }} />);
    expect((screen.getByLabelText('Agent') as HTMLSelectElement).value).toBe('missing');
    expect((screen.getByRole('button', { name: 'Start working on this' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('offers unpinned recipes under each login, pinned recipes under their owner and routes under Default', () => {
    const choices = launchChoices(providers);
    expect(choices.filter(row => row.configurationId === 'launch/free').map(row => row.loginId)).toEqual(['default', 'work', 'guest']);
    expect(choices.filter(row => row.configurationId === 'launch/work').map(row => row.loginId)).toEqual(['work']);
    expect(choices.filter(row => row.configurationId === 'launch/route').map(row => row.loginId)).toEqual(['default']);
    expect(choices.find(row => row.loginId === 'guest')?.unavailableReason).toContain('Not signed in');
    expect(launchFields(choices.find(row => row.id === loginChoiceId('launch/free', 'work')))).toEqual({
      provider: 'codex', configuration_id: 'launch/free', login_id: 'work',
    });
    expect(launchFields(null)).toEqual({});
  });

  it('locks unsigned choices and pending sheets, and Escape dismisses only when idle', () => {
    const close = vi.fn(); const choose = vi.fn();
    const props = { label: 'Launch', choices: launchChoices(providers), error: null, onClose: close, onChoose: choose };
    const view = render(<SpawnMenuSheet {...props} />);
    expect((screen.getByRole('button', { name: /Codex · Guest/ }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Codex · Work', exact: true }));
    expect(choose).toHaveBeenCalledWith(expect.objectContaining({ provider: 'codex', loginId: 'work' }));
    view.rerender(<SpawnMenuSheet {...props} dismissible={false} />);
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(close).not.toHaveBeenCalled();
    expect((screen.getByRole('button', { name: 'Codex · Work', exact: true }) as HTMLButtonElement).disabled).toBe(true);
    view.rerender(<SpawnMenuSheet {...props} />);
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(close).toHaveBeenCalledOnce();
  });

  it('retains the mesh preset login through Capture and posts the selected account', async () => {
    const fetcher = vi.fn((_url: string, _init?: RequestInit) => Promise.resolve(response({ outcome: 'active', node: { id: 17 } })));
    vi.stubGlobal('fetch', fetcher);
    render(<CaptureIdea meshes={[{ id: 3, name: 'Repo' } as Mesh]} providers={providers}
      preset={{ meshId: 3, launchId: loginChoiceId('launch/work', 'work') }} onStarted={vi.fn()} onAuthFailed={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Your idea'), { target: { value: 'Review the mobile launch' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start working on this' }));
    await waitFor(() => expect(fetcher).toHaveBeenCalledOnce());
    expect(JSON.parse(String(fetcher.mock.calls[0]?.[1]?.body))).toMatchObject({
      mesh_id: 3, provider: 'codex', configuration_id: 'launch/work', login_id: 'work',
    });
  });

  it('sends a built-in reviewer login and omits it when a saved Circuit owns the reviewer', async () => {
    const fetcher = vi.fn((url: string, init?: RequestInit) => Promise.resolve(response(init?.method === 'POST'
      ? { ok: true, run_id: 7 } : url === '/api/providers' ? providers
        : [{ circuit_id: null, name: 'Built-in Review Blueprint' }, { circuit_id: 9, name: 'Team review' }])));
    vi.stubGlobal('fetch', fetcher);
    render(<StartReview nodeId={17} onAuthFailed={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
    const picker = await screen.findByLabelText('Reviewer launch configuration');
    await screen.findByRole('option', { name: 'launch/work · Work', exact: true });
    fireEvent.change(picker, { target: { value: loginChoiceId('launch/work', 'work') } });
    fireEvent.click(screen.getByRole('button', { name: 'Start review', exact: true }));
    await screen.findByText('Review started. An existing live run is kept.');
    expect(JSON.parse(String(fetcher.mock.calls.find(([, init]) => init?.method === 'POST')?.[1]?.body))).toEqual({
      circuit_id: null, max_rounds: 3, reviewer_provider: 'launch/work', reviewer_login_id: 'work',
    });
    fetcher.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
    await screen.findByRole('option', { name: 'Team review' });
    fireEvent.change(screen.getByLabelText('Review Circuit'), { target: { value: '9' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start review', exact: true }));
    await screen.findByText('Review started. An existing live run is kept.');
    expect(JSON.parse(String(fetcher.mock.calls.find(([, init]) => init?.method === 'POST')?.[1]?.body))).toEqual({ circuit_id: 9, max_rounds: 3 });
  });
});
