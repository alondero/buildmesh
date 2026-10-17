import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { StartReview } from '../../src/mobile/screens/StartReview';

const response = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
afterEach(() => vi.unstubAllGlobals());

it('loads same-node choices, confirms a built-in review and prevents duplicate submissions', async () => {
  let finish!: (response: Response) => void;
  const fetcher = vi.fn((_url: string, init?: RequestInit) => init?.method === 'POST'
    ? new Promise<Response>((resolve) => { finish = resolve; })
    : Promise.resolve(response([{ circuit_id: null, name: 'Built-in Review Blueprint' }, { circuit_id: 9, name: 'Team review' }])));
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
  expect(JSON.parse(String(fetcher.mock.calls[1][1]?.body))).toEqual({ circuit_id: null, max_rounds: 4 });
  await act(async () => finish(response({ ok: true, run_id: 7 })));
  expect(await screen.findByRole('status')).toHaveProperty('textContent', expect.stringContaining('Review started'));
  expect(screen.queryByRole('dialog')).toBeNull();
});

it('sends the saved review id, leaves refusal in the sheet and retains pairing', async () => {
  const auth = vi.fn();
  const fetcher = vi.fn((_url: string, init?: RequestInit) => Promise.resolve(init?.method === 'POST'
    ? response({ error: 'Resume the agent before starting a review.' }, 409)
    : response([{ circuit_id: null, name: 'Built-in Review Blueprint' }, { circuit_id: 9, name: 'Team review' }])));
  vi.stubGlobal('fetch', fetcher);
  render(<StartReview nodeId={17} onAuthFailed={auth} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  await screen.findByText('Team review');
  fireEvent.change(screen.getByLabelText('Review rounds'), { target: { value: '0' } });
  expect((screen.getByRole('button', { name: 'Start review', exact: true }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText('Review Circuit'), { target: { value: '9' } });
  expect(screen.queryByLabelText('Review rounds')).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Start review', exact: true }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Resume the agent before starting a review.');
  expect(JSON.parse(String(fetcher.mock.calls[1][1]?.body))).toEqual({ circuit_id: 9, max_rounds: 3 });
  expect(auth).not.toHaveBeenCalled();
  expect(screen.getByRole('dialog')).toBeTruthy();
});

it('ignores a late rejected start after the screen unmounts', async () => {
  let finish!: (response: Response) => void;
  vi.stubGlobal('fetch', vi.fn((_url: string, init?: RequestInit) => init?.method === 'POST'
    ? new Promise<Response>((resolve) => { finish = resolve; })
    : Promise.resolve(response([{ circuit_id: null, name: 'Built-in Review Blueprint' }]))));
  const auth = vi.fn();
  const view = render(<StartReview nodeId={17} onAuthFailed={auth} />);
  fireEvent.click(screen.getByRole('button', { name: 'Start review…' }));
  await screen.findByText('Built-in Review Blueprint');
  fireEvent.click(screen.getByRole('button', { name: 'Start review', exact: true }));
  view.unmount();
  await act(async () => finish(response({ error: 'revoked' }, 403)));
  expect(auth).not.toHaveBeenCalled();
});
