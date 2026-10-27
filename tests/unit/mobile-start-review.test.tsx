import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { StartReview } from '../../src/mobile/screens/StartReview';

const response = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
afterEach(() => vi.unstubAllGlobals());

it('offers a reviewer launch configuration and sends its identity with the built-in review', async () => {
  const fetcher = vi.fn((url: string, init?: RequestInit) => Promise.resolve(response(
    init?.method === 'POST' ? { ok: true, run_id: 7 }
      : url === '/api/providers' ? [{
        id: 'launch/reviewer', label: 'Careful reviewer', harness_id: 'codex', group_key: 'codex', is_proxied: false,
        configuration: { id: 'launch/reviewer', name: 'Careful reviewer', spawn_option_id: 'codex', harness_id: 'codex' },
      }]
        : [{ circuit_id: null, name: 'Built-in Review Blueprint' }],
  )));
  vi.stubGlobal('fetch', fetcher);
  render(<StartReview nodeId={17} onAuthFailed={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  const picker = await screen.findByLabelText('Reviewer launch configuration');
  fireEvent.change(picker, { target: { value: 'launch/reviewer' } });
  fireEvent.click(screen.getByRole('button', { name: 'Start review', exact: true }));
  await screen.findByText('Review started. An existing live run is kept.');
  const post = fetcher.mock.calls.find(([, init]) => init?.method === 'POST');
  expect(JSON.parse(String(post?.[1]?.body))).toEqual({ circuit_id: null, max_rounds: 3, reviewer_provider: 'launch/reviewer' });
});

it('loads same-node choices, confirms a built-in review and prevents duplicate submissions', async () => {
  let finish!: (response: Response) => void;
  const fetcher = vi.fn((url: string, init?: RequestInit) => init?.method === 'POST'
    ? new Promise<Response>((resolve) => { finish = resolve; })
    : Promise.resolve(response(url === '/api/providers' ? [] : [{ circuit_id: null, name: 'Built-in Review Blueprint' }, { circuit_id: 9, name: 'Team review' }])));
  vi.stubGlobal('fetch', fetcher);
  render(<StartReview nodeId={17} onAuthFailed={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  await screen.findByText('Team review');
  expect(fetcher.mock.calls[0][0]).toBe('/api/nodes/17/reviews');
  fireEvent.change(screen.getByLabelText('Review rounds'), { target: { value: '4' } });
  fireEvent.click(screen.getByRole('button', { name: 'Start review', exact: true }));
  fireEvent.click(screen.getByRole('button', { name: 'Starting…' }));
  await waitFor(() => expect(finish).toBeTypeOf('function'));
  expect(fetcher.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(1);
  expect(JSON.parse(String(fetcher.mock.calls.find(([, init]) => init?.method === 'POST')?.[1]?.body))).toEqual({ circuit_id: null, max_rounds: 4 });
  await act(async () => finish(response({ ok: true, run_id: 7 })));
  expect(await screen.findByRole('status')).toHaveProperty('textContent', expect.stringContaining('Review started'));
  expect(screen.queryByRole('dialog')).toBeNull();
});

it('sends the saved review id, leaves refusal in the sheet and retains pairing', async () => {
  const auth = vi.fn();
  const fetcher = vi.fn((url: string, init?: RequestInit) => Promise.resolve(init?.method === 'POST'
    ? response({ error: 'Resume the agent before starting a review.' }, 409)
    : response(url === '/api/providers' ? [{ id: 'codex', label: 'Codex', harness_id: 'codex', group_key: 'codex', is_proxied: false }] : [{ circuit_id: null, name: 'Built-in Review Blueprint' }, { circuit_id: 9, name: 'Team review' }])));
  vi.stubGlobal('fetch', fetcher);
  render(<StartReview nodeId={17} onAuthFailed={auth} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  await screen.findByText('Team review');
  fireEvent.change(screen.getByLabelText('Reviewer launch configuration'), { target: { value: 'codex' } });
  fireEvent.change(screen.getByLabelText('Review rounds'), { target: { value: '0' } });
  expect((screen.getByRole('button', { name: 'Start review', exact: true }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText('Review Circuit'), { target: { value: '9' } });
  expect(screen.queryByLabelText('Review rounds')).toBeNull();
  expect(screen.queryByLabelText('Reviewer launch configuration')).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Start review', exact: true }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Resume the agent before starting a review.');
  expect(JSON.parse(String(fetcher.mock.calls.find(([, init]) => init?.method === 'POST')?.[1]?.body))).toEqual({ circuit_id: 9, max_rounds: 3 });
  expect(auth).not.toHaveBeenCalled();
  expect(screen.getByRole('dialog')).toBeTruthy();
});

it('ignores a late rejected start after the screen unmounts', async () => {
  let finish!: (response: Response) => void;
  vi.stubGlobal('fetch', vi.fn((url: string, init?: RequestInit) => init?.method === 'POST'
    ? new Promise<Response>((resolve) => { finish = resolve; })
    : Promise.resolve(response(url === '/api/providers' ? [] : [{ circuit_id: null, name: 'Built-in Review Blueprint' }]))));
  const auth = vi.fn();
  const view = render(<StartReview nodeId={17} onAuthFailed={auth} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  await screen.findByText('Built-in Review Blueprint');
  fireEvent.click(screen.getByRole('button', { name: 'Start review', exact: true }));
  view.unmount();
  await act(async () => finish(response({ error: 'revoked' }, 403)));
  expect(auth).not.toHaveBeenCalled();
});

it('keeps unavailable reviewers visible, excludes terminals and bare routes, and preserves backend order', async () => {
  vi.stubGlobal('fetch', vi.fn((url: string) => Promise.resolve(response(url === '/api/providers' ? [
    { id: 'codex', label: 'Codex', harness_id: 'codex', group_key: 'codex', is_proxied: false },
    { id: 'launch/missing', label: 'Offline reviewer', harness_id: 'codex', group_key: 'codex', is_proxied: false, configuration: { id: 'launch/missing', name: 'Offline reviewer' }, unavailable_reason: 'Route is disabled' },
    { id: 'codex:proxy', label: 'Bare route', harness_id: 'codex', group_key: 'codex', is_proxied: true },
    { id: 'freebuff', label: 'Freebuff', harness_id: 'freebuff', group_key: 'freebuff', is_proxied: false },
    { id: 'terminal', label: 'Terminal', harness_id: 'terminal', group_key: 'terminal', is_proxied: false },
  ] : [{ circuit_id: null, name: 'Built-in Review Blueprint' }]))));
  render(<StartReview nodeId={17} onAuthFailed={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  await screen.findByRole('option', { name: 'Codex' });
  const options = [...(screen.getByLabelText('Reviewer launch configuration') as HTMLSelectElement).options];
  expect(options.map((option) => option.value)).toEqual(['', 'codex', 'launch/missing', 'freebuff']);
  expect(options[2].disabled).toBe(true);
  expect(options[2].text).toContain('Route is disabled');
  expect(options[3].disabled).toBe(true);
  expect(options[3].text).toContain('No turn-completion signal');
});

it('offers a retry for a failed launch menu while default review remains available', async () => {
  let reads = 0;
  const auth = vi.fn();
  const fetcher = vi.fn((url: string) => Promise.resolve(url === '/api/providers'
    ? ++reads === 1 ? response({ error: 'Menu unavailable' }, 503) : response([{ id: 'codex', label: 'Codex', harness_id: 'codex', group_key: 'codex', is_proxied: false }])
    : response([{ circuit_id: null, name: 'Built-in Review Blueprint' }])));
  vi.stubGlobal('fetch', fetcher);
  render(<StartReview nodeId={17} onAuthFailed={auth} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', expect.stringContaining('Menu unavailable'));
  expect((screen.getByRole('button', { name: 'Start review', exact: true }) as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(screen.getByRole('button', { name: 'Retry launch configurations' }));
  await screen.findByRole('option', { name: 'Codex' });
  expect(screen.queryByRole('alert')).toBeNull();
  expect(auth).not.toHaveBeenCalled();
});

it('aborts closed launch reads and ignores a stale response after reopening', async () => {
  const pending: { resolve: (value: Response) => void; signal?: AbortSignal | null }[] = [];
  vi.stubGlobal('fetch', vi.fn((url: string, init?: RequestInit) => url === '/api/providers'
    ? new Promise<Response>((resolve) => { pending.push({ resolve, signal: init?.signal }); })
    : Promise.resolve(response([{ circuit_id: null, name: 'Built-in Review Blueprint' }]))));
  render(<StartReview nodeId={17} onAuthFailed={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  await screen.findByLabelText('Reviewer launch configuration');
  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  expect(pending[0].signal?.aborted).toBe(true);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  await act(async () => pending[1].resolve(response([{ id: 'codex', label: 'Codex', harness_id: 'codex', group_key: 'codex', is_proxied: false }])));
  await act(async () => pending[0].resolve(response([{ id: 'claude', label: 'Old menu', harness_id: 'claude', group_key: 'claude', is_proxied: false }])));
  expect(screen.getByRole('option', { name: 'Codex' })).toBeTruthy();
  expect(screen.queryByRole('option', { name: 'Old menu' })).toBeNull();
});
