/**
 * App Settings — the opt-in DevTools port (issue #2836).
 *
 * The port is a startup switch, so the row under test has to say two things
 * that are easy to conflate: what the user asked for (the field), and what
 * is actually listening (the status line).
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';

const tauriMocks = vi.hoisted(() => ({
  getAppPreferences: vi.fn(),
  listProviders: vi.fn(),
  getProviderAccounts: vi.fn(),
  getKeyedFirstClassCatalog: vi.fn(),
  getCoordinatorStatus: vi.fn(),
  listDeviceSessions: vi.fn(),
  getNetworkStatus: vi.fn(),
  setAppDiagCdpPort: vi.fn(),
  getDevtoolsStatus: vi.fn(),
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/lib/tauri')>()),
  getAppPreferences: tauriMocks.getAppPreferences,
  listProviders: tauriMocks.listProviders,
  getProviderAccounts: tauriMocks.getProviderAccounts,
  getKeyedFirstClassCatalog: tauriMocks.getKeyedFirstClassCatalog,
  getCoordinatorStatus: tauriMocks.getCoordinatorStatus,
  listDeviceSessions: tauriMocks.listDeviceSessions,
  getNetworkStatus: tauriMocks.getNetworkStatus,
  setAppDiagCdpPort: tauriMocks.setAppDiagCdpPort,
}));

// The DevTools status has its own facet module, so it is mocked on its own
// rather than through the barrel.
vi.mock('../../src/lib/tauri/devtools', () => ({
  getDevtoolsStatus: tauriMocks.getDevtoolsStatus,
}));

import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { useDevtoolsStore } from '../../src/stores/devtoolsStore';

beforeEach(() => {
  // A known, closed status: the store keeps the first read, so a status set
  // here is the one the row reports (no fetch race).
  useDevtoolsStore.setState({ status: { port: null, source: 'off' } });
  tauriMocks.listProviders.mockReset().mockResolvedValue([]);
  tauriMocks.getProviderAccounts.mockReset().mockResolvedValue([]);
  tauriMocks.getKeyedFirstClassCatalog.mockReset().mockResolvedValue([]);
  tauriMocks.getCoordinatorStatus.mockReset().mockResolvedValue({ enabled: false, has_token: false });
  tauriMocks.listDeviceSessions.mockReset().mockResolvedValue([]);
  tauriMocks.getNetworkStatus.mockReset().mockResolvedValue({
    lan_exposure_enabled: false,
    tls_active: false,
    exposed_interfaces: [],
  });
  tauriMocks.setAppDiagCdpPort.mockReset().mockResolvedValue(undefined);
  tauriMocks.getDevtoolsStatus.mockReset().mockResolvedValue({ port: null, source: 'off' });
});

const PORT_FIELD = 'DevTools port';
const WORKTREE_FIELD = 'Worktree directory';

const BASE_PREFERENCES = {
  default_provider: null,
  naming_provider: null,
  circuit_agent_pool_size: null,
  harness_defaults: {},
  provider_pairings: [],
  confirm_before_quit: true,
};

/**
 * Render the settings modal with a preferences payload and wait until the
 * pane has adopted it.
 *
 * The payload arrives asynchronously, and the worktree directory carries a
 * value the pane applies during render — so it is the signal that the
 * payload landed. Without that wait, an assertion on a payload-derived value
 * is a race with the load.
 */
async function renderModal(preferences: Record<string, unknown> = {}) {
  tauriMocks.getAppPreferences.mockReset().mockResolvedValue({
    ...BASE_PREFERENCES,
    worktree_directory: 'worktrees',
    diag_cdp_port: null,
    ...preferences,
  });
  render(<AppSettingsModal onClose={() => {}} />);
  await screen.findByLabelText(PORT_FIELD);
  await waitFor(() =>
    expect((screen.getByLabelText(WORKTREE_FIELD) as HTMLInputElement).value).toBe('worktrees'),
  );
}

describe('App Settings — DevTools port (issue #2836)', () => {
  it('is empty by default, because the port is off by default', async () => {
    await renderModal();
    expect(screen.getByLabelText(PORT_FIELD)).toHaveProperty('value', '');
    expect(screen.getByTestId('devtools-port-status').textContent).toBe('Closed.');
  });

  it('does not claim the port is closed when the status could not be read', async () => {
    // A failed read leaves the status unknown. Saying "Closed" would be a
    // claim the app has no evidence for — and it is the claim that matters,
    // because an open port lets any local program drive the window.
    tauriMocks.getDevtoolsStatus.mockRejectedValue(new Error('command unavailable'));
    useDevtoolsStore.setState({ status: null });
    await renderModal();
    expect(screen.getByTestId('devtools-port-status').textContent).toBe('Status unknown.');
  });

  it('saves the port the user typed', async () => {
    await renderModal();
    const field = screen.getByLabelText(PORT_FIELD);
    await act(async () => {
      fireEvent.change(field, { target: { value: '9222' } });
      fireEvent.blur(field);
    });
    await waitFor(() => expect(tauriMocks.setAppDiagCdpPort).toHaveBeenCalledWith(9222));
  });

  it('refuses a port that is out of range, and saves no port', async () => {
    await renderModal();
    const field = screen.getByLabelText(PORT_FIELD);
    await act(async () => {
      fireEvent.change(field, { target: { value: '80' } });
      fireEvent.blur(field);
    });
    await waitFor(() => expect(screen.getByText(/whole number/)).toBeTruthy());
    expect(tauriMocks.setAppDiagCdpPort).not.toHaveBeenCalled();
  });

  it('turns the port off when the field is cleared', async () => {
    await renderModal({ diag_cdp_port: 9222 });
    const field = screen.getByLabelText(PORT_FIELD) as HTMLInputElement;
    expect(field.value).toBe('9222');
    await act(async () => {
      fireEvent.change(field, { target: { value: '' } });
      fireEvent.blur(field);
    });
    await waitFor(() => expect(tauriMocks.setAppDiagCdpPort).toHaveBeenCalledWith(null));
  });

  it('says a saved port only opens on the next launch', async () => {
    await renderModal({ diag_cdp_port: 9222 });
    expect(screen.getByTestId('devtools-port-status').textContent).toBe(
      'Closed. Restart Buildmesh to open port 9222.',
    );
  });

  it('says which port is open when this process has one', async () => {
    useDevtoolsStore.setState({ status: { port: 9223, source: 'environment' } });
    await renderModal();
    expect(screen.getByTestId('devtools-port-status').textContent).toBe(
      'Open on 127.0.0.1 (port 9223).',
    );
  });

  it('names both ports when the open one is not the saved one', async () => {
    useDevtoolsStore.setState({ status: { port: 9223, source: 'environment' } });
    await renderModal({ diag_cdp_port: 9222 });
    expect(screen.getByTestId('devtools-port-status').textContent).toBe(
      'Open on 127.0.0.1 (port 9223). Restart Buildmesh to switch to port 9222.',
    );
  });
});
