import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import type { NetworkStatus } from '../../src/types/generated/NetworkStatus';
import type { CertChainStatus } from '../../src/types/generated/CertChainStatus';
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

const SAMPLE_CERT: CertChainStatus = {
  root_fingerprint_sha256:
    'AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99',
  leaf_fingerprint_sha256:
    '11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11',
  leaf_issuer: 'CN=Buildmesh Dev Root CA',
  valid_until: '2035-01-01 00:00:00',
  cert_path: 'C:\\Users\\alond\\AppData\\Roaming\\com.alond.buildmesh\\tls\\ca.der',
};

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

  it('prefers an IPv4 TLS bind over an IPv6 one so the phone gets a reachable URL', () => {
    // The OS can enumerate an IPv6 interface ahead of the IPv4 LAN address; a
    // bracketed IPv6 (especially link-local) in the QR makes the phone browser
    // bail with ERR_INVALID_ARGUMENT. IPv4 LAN is the canonical "phone on the
    // same Wi-Fi" target, so it must win regardless of enumeration order.
    const result = buildRemoteAccessUrl(
      status({
        exposed_interfaces: [
          { address: '[2001:db8::1]:1992', tls: true },
          { address: '192.168.1.10:1992', tls: true },
        ],
      }),
      '192.168.1.10',
      't',
    );
    expect(result.url).toBe('https://192.168.1.10:1992/?token=t');
    expect(result.host).toBe('192.168.1.10:1992');
  });

  it('uses an IPv6 bind only when no IPv4 interface is realized', () => {
    const result = buildRemoteAccessUrl(
      status({
        exposed_interfaces: [{ address: '[2001:db8::1]:1992', tls: true }],
      }),
      '192.168.1.10',
      't',
    );
    expect(result.url).toBe('https://[2001:db8::1]:1992/?token=t');
    expect(result.reachable).toBe(true);
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

  function mockBackend(
    s: NetworkStatus,
    opts: { localIp?: string; cert?: CertChainStatus | null } = {},
  ) {
    const localIp = opts.localIp ?? '192.168.1.10';
    // Default to returning the sample cert; tests that want to exercise the
    // "fetch failed" path pass `cert: null`.
    const cert = opts.cert === undefined ? SAMPLE_CERT : opts.cert;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_root_token':
          return Promise.resolve('root-tok');
        case 'get_local_ip':
          return Promise.resolve(localIp);
        case 'get_network_status':
          return Promise.resolve(s);
        case 'get_cert_chain_status':
          return cert === null
            ? Promise.reject(new Error('cert status unavailable'))
            : Promise.resolve(cert);
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

  // --- issue #635: cert status surface ------------------------------------
  // The QR modal surfaces the server's current root CA fingerprint so a user
  // whose installed root is stale (LAN-IP change forced a regen) can see the
  // mismatch and re-install without reaching for `openssl`. The fingerprint is
  // always shown when the status fetch succeeds; the "Re-install" affordance
  // expands to reveal OS-specific install steps.

  it('renders the server root fingerprint below the QR', async () => {
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    const fp = await screen.findByTestId('remote-access-cert-fingerprint');
    expect(fp.textContent).toBe(SAMPLE_CERT.root_fingerprint_sha256);
  });

  it('does not crash when the cert status fetch fails', async () => {
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
      { cert: null },
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    // QR still renders, fingerprint section is absent.
    await screen.findByText(/192\.168\.1\.10:1992/);
    expect(screen.queryByTestId('remote-access-cert-fingerprint')).toBeNull();
  });

  it('expands the Re-install section when the toggle is clicked', async () => {
    const user = userEvent.setup();
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    // Wait for the section to render, then click the toggle.
    const toggle = await screen.findByTestId('remote-access-cert-reinstall-toggle');
    expect(screen.queryByTestId('remote-access-cert-reinstall')).toBeNull();

    await user.click(toggle);

    const section = await screen.findByTestId('remote-access-cert-reinstall');
    expect(section.textContent).toMatch(/Android/);
    expect(section.textContent).toMatch(/iOS/);
    expect(
      screen.getByTestId('remote-access-cert-path').textContent,
    ).toBe(SAMPLE_CERT.cert_path);
  });

  it('copies the cert path to the clipboard when Copy is clicked', async () => {
    // userEvent.setup() installs its own jsdom clipboard stub (the same
    // pattern as `coordinator-settings.test.tsx:98`). We assert by reading
    // the clipboard back, no manual stub required.
    const user = userEvent.setup();
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await user.click(
      await screen.findByTestId('remote-access-cert-reinstall-toggle'),
    );
    await user.click(await screen.findByTestId('remote-access-cert-copy'));

    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe(SAMPLE_CERT.cert_path),
    );
  });
});
