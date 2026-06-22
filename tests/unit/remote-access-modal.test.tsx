import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import type { NetworkStatus } from '../../src/types/generated/NetworkStatus';
import {
  RemoteAccessModal,
  buildRemoteAccessUrl,
} from '../../src/components/RemoteAccess/RemoteAccessModal';

// The QR library touches a <canvas> jsdom can't render; we only care about the
// URL it's handed, so capture the argument instead of producing a real image.
const toDataURL = vi.fn().mockResolvedValue('data:image/png;base64,stub');
vi.mock('qrcode', () => ({
  default: { toDataURL: (...args: unknown[]) => toDataURL(...args) },
}));

function status(overrides: Partial<NetworkStatus>): NetworkStatus {
  return {
    lan_exposure_enabled: true,
    port: 1992,
    tls_active: true,
    exposed_interfaces: [],
    ...overrides,
  };
}

describe('buildRemoteAccessUrl (issue: stale http:// QR scheme)', () => {
  it('uses https:// and the realized bind address when a TLS interface is bound', () => {
    const result = buildRemoteAccessUrl(
      status({
        exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }],
      }),
      '192.168.1.10',
      'root-tok',
    );
    expect(result.url).toBe('https://192.168.1.10:1992/?token=root-tok');
    expect(result.host).toBe('192.168.1.10:1992');
    expect(result.reachable).toBe(true);
  });

  it('honours the realized port from the bind, not a hardcoded 1992', () => {
    const result = buildRemoteAccessUrl(
      status({
        port: 1994,
        exposed_interfaces: [{ address: '192.168.1.10:1994', tls: true }],
      }),
      '192.168.1.10',
      't',
    );
    expect(result.url).toBe('https://192.168.1.10:1994/?token=t');
  });

  it('matches the scheme to a plain (non-TLS) realized bind', () => {
    const result = buildRemoteAccessUrl(
      status({
        tls_active: false,
        exposed_interfaces: [{ address: '192.168.1.10:1992', tls: false }],
      }),
      '192.168.1.10',
      't',
    );
    expect(result.url).toBe('http://192.168.1.10:1992/?token=t');
  });

  it('falls back to the discovered IP + reported port over http when nothing is realized, and flags it unreachable', () => {
    const result = buildRemoteAccessUrl(
      status({ tls_active: false, exposed_interfaces: [] }),
      '192.168.1.50',
      't',
    );
    expect(result.url).toBe('http://192.168.1.50:1992/?token=t');
    expect(result.reachable).toBe(false);
  });
});

describe('RemoteAccessModal', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    toDataURL.mockClear();
  });

  function mockBackend(s: NetworkStatus, localIp = '192.168.1.10') {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_root_token':
          return Promise.resolve('root-tok');
        case 'get_local_ip':
          return Promise.resolve(localIp);
        case 'get_network_status':
          return Promise.resolve(s);
        default:
          return Promise.resolve({});
      }
    });
  }

  it('encodes an https URL in the QR when LAN exposure is realized over TLS', async () => {
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await waitFor(() => expect(toDataURL).toHaveBeenCalled());
    expect(toDataURL.mock.calls[0][0]).toBe(
      'https://192.168.1.10:1992/?token=root-tok',
    );
    expect(await screen.findByText(/192\.168\.1\.10:1992/)).toBeTruthy();
  });

  it('warns instead of showing a dead URL when exposure is enabled but no interface is bound', async () => {
    mockBackend(status({ tls_active: false, exposed_interfaces: [] }));
    render(<RemoteAccessModal onClose={() => {}} />);

    const warning = await screen.findByTestId('remote-access-warning');
    expect(warning.textContent).toMatch(/no.*interface|not.*exposed|reach/i);
  });
});
