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

// The install fallback link opens the URL in the host's default browser via
// `openUrl()` (issue #810) — NOT `window.location.href`, which would navigate
// the Tauri WebView itself. Capture the argument to assert the route.
const openUrl = vi.fn().mockResolvedValue(undefined);
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: (...args: unknown[]) => openUrl(...args),
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
    openUrl.mockClear();
  });

  function mockBackend(
    s: NetworkStatus,
    opts: {
      localIp?: string;
      cert?: CertChainStatus | null;
      mobileconfig?: string | null;
    } = {},
  ) {
    const localIp = opts.localIp ?? '192.168.1.10';
    // Default to returning the sample cert; tests that want to exercise the
    // "fetch failed" path pass `cert: null`.
    const cert = opts.cert === undefined ? SAMPLE_CERT : opts.cert;
    // Issue #713: default to a sample base64 string for the iOS
    // `.mobileconfig` path so existing tests don't need to opt in.
    // Tests that want to exercise the iOS-failed path pass
    // `mobileconfig: null`.
    const mobileconfig =
      opts.mobileconfig === undefined
        ? // A handful of ASCII chars is fine for the QR mock — we only
          // assert on the data: URL prefix, not the base64 contents.
          'QUJDREVGRw=='
        : opts.mobileconfig;
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
        case 'get_root_cert_mobileconfig':
          return mobileconfig === null
            ? Promise.reject(new Error('mobileconfig unavailable'))
            : Promise.resolve(mobileconfig);
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

  // --- #702 follow-up: install-QR payload is the HTTPS install URL ------
  // The previous 3-button picker navigated the DESKTOP's WebView to
  // /install-cert.der — a phone user couldn't click those buttons. The
  // replacement renders the install URL inside a SECOND QR (on the
  // "Install cert" tab) so the phone scans it directly. The previous
  // main-branch attempt embedded `data:application/x-x509-ca-cert;
  // base64,…` (PR #712); that fails on Android because the QR scanner's
  // intent system has no handler for `data:` URIs of cert MIME types
  // and shows "no apps can use this data". An HTTPS URL pointing at the
  // existing /install-cert.der route works on every OS: the browser
  // handles the TLS warning once, the .der downloads with the correct
  // Content-Type, and Chrome/Safari routes it into the OS cert
  // installer.
  //
  // The two QRs live behind tabs (Connect default | Install cert) so
  // each can be rendered at the full 384px scan-friendly size. A
  // side-by-side layout squeezed both into 160px which failed at scan
  // distance — see commit history on the modal for that experiment.

  it('shows all three tabs when LAN is realized and both install paths succeed', async () => {
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    // All three tab buttons appear. Connect is the default — its QR is
    // in the DOM, the two install QRs are not until the user clicks
    // their tab. (Issue #713: a third iOS install tab joins the existing
    // Connect | Install — Android bar.)
    const connectTab = await screen.findByTestId('remote-access-tab-connect');
    const installAndroidTab = await screen.findByTestId('remote-access-tab-install-android');
    const installIosTab = await screen.findByTestId('remote-access-tab-install-ios');
    expect(connectTab.getAttribute('aria-selected')).toBe('true');
    expect(installAndroidTab.getAttribute('aria-selected')).toBe('false');
    expect(installIosTab.getAttribute('aria-selected')).toBe('false');

    await waitFor(() => expect(screen.getByTestId('remote-access-connect-qr')).toBeTruthy());
    expect(screen.queryByTestId('remote-access-install-android-qr')).toBeNull();
    expect(screen.queryByTestId('remote-access-install-ios-qr')).toBeNull();
  });

  it('swaps to the Android install QR when the Install — Android tab is clicked', async () => {
    const user = userEvent.setup();
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    // Wait for connect QR to render (proves the initial load settled),
    // then click the Android install tab. Install QR should appear;
    // connect QR should leave the DOM (single-active-QR design keeps the
    // inactive <img> out of the DOM — a stray connect QR visible after
    // the user switches tabs would be a screen-grab hazard).
    await screen.findByTestId('remote-access-connect-qr');
    await user.click(await screen.findByTestId('remote-access-tab-install-android'));

    await waitFor(() =>
      expect(screen.getByTestId('remote-access-install-android-qr')).toBeTruthy(),
    );
    expect(screen.queryByTestId('remote-access-connect-qr')).toBeNull();

    const installAndroidTab = await screen.findByTestId('remote-access-tab-install-android');
    expect(installAndroidTab.getAttribute('aria-selected')).toBe('true');
  });

  it('swaps to the iOS install QR when the Install — iOS tab is clicked', async () => {
    // Sibling to the Android tab test above; locks the iOS tab's
    // mount/unmount behaviour on tab-click (issue #713).
    const user = userEvent.setup();
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await screen.findByTestId('remote-access-connect-qr');
    await user.click(await screen.findByTestId('remote-access-tab-install-ios'));

    await waitFor(() => expect(screen.getByTestId('remote-access-install-ios-qr')).toBeTruthy());
    expect(screen.queryByTestId('remote-access-connect-qr')).toBeNull();

    const installIosTab = await screen.findByTestId('remote-access-tab-install-ios');
    expect(installIosTab.getAttribute('aria-selected')).toBe('true');
  });

  it('renders the active QR at full size (w-96 h-96) for scan-friendly distance', async () => {
    // The side-by-side experiment at w-40 h-40 was unreadable from
    // across the room; tabs give each QR the full 384px width. Lock the
    // size class so a future refactor that resizes for "balance" can't
    // silently regress scan distance.
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    const { container } = render(<RemoteAccessModal onClose={() => {}} />);

    await screen.findByTestId('remote-access-connect-qr');
    const img = container.querySelector(
      '[data-testid="remote-access-connect-qr"] img',
    ) as HTMLImageElement | null;
    expect(img).toBeTruthy();
    expect(img!.className).toMatch(/w-96/);
    expect(img!.className).toMatch(/h-96/);
  });

  it('encodes all three QRs at the same pixel size as their render box (no upscale blur)', async () => {
    // Regression guard for code-review finding: the install-QR was
    // encoded at width=256 but rendered at w-96 (384px), causing the
    // browser to upscale the raster and blur the QR modules. All three
    // QRs (issue #713 adds the iOS install-QR) MUST encode at width=384
    // to match the w-96 h-96 render box. Asserts on the `width` option
    // of each `QRCode.toDataURL` call, order-agnostic (parallel
    // Promise.allSettled).
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await waitFor(() => expect(toDataURL.mock.calls.length).toBeGreaterThanOrEqual(3));

    const widths = toDataURL.mock.calls.map(c => {
      const opts = c[1] as { width?: number } | undefined;
      return opts?.width;
    });
    // Three calls present (connect + Android install + iOS install) and
    // all at 384.
    expect(widths).toHaveLength(3);
    expect(widths).toEqual([384, 384, 384]);
  });

  it('encodes the Android install-QR payload as an https URL to /install-cert.der on the realized bind', async () => {
    const user = userEvent.setup();
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    // Wait for all three QR generation calls (parallel Promise.allSettled).
    await waitFor(() => expect(toDataURL.mock.calls.length).toBeGreaterThanOrEqual(3));
    // Switch to the Android install tab so the install QR is rendered
    // — all QRs are still in mock.calls regardless of which tab is
    // active.
    await user.click(await screen.findByTestId('remote-access-tab-install-android'));

    // Find the install-QR call — order-agnostic, since the QR
    // generations run in parallel and may resolve in any order.
    const payloads = toDataURL.mock.calls.map(c => c[0]);
    const installPayload = payloads.find(
      (p): p is string =>
        typeof p === 'string' && p.includes('/install-cert.der'),
    );
    expect(installPayload).toBe('https://192.168.1.10:1992/install-cert.der');
    // Regression guard for #702: the previous main-branch attempt encoded
    // the cert as a `data:application/x-x509-ca-cert;base64,…` payload.
    // Android's QR scanner has no intent filter for that scheme+MIME and
    // shows "no apps can use this data". A future refactor that swaps back
    // to a data: URL would silently break Android — this assertion is
    // the load-bearing guard against that regression.
    expect(payloads.some(p => typeof p === 'string' && p.startsWith('data:application/x-x509-ca-cert'))).toBe(false);
  });

  it('encodes the iOS install-QR payload as a data:application/x-apple-aspen-config;base64 URL', async () => {
    // Sibling to the Android install-QR test above (issue #713).
    // The base64 suffix is the value `getRootCertMobileconfig()` resolves
    // to (mocked here); the data: URL prefix is what Safari intercepts
    // for `.mobileconfig` install. Lock the prefix so a refactor that
    // swaps to the HTTPS install path can't silently break iOS.
    const user = userEvent.setup();
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await waitFor(() => expect(toDataURL.mock.calls.length).toBeGreaterThanOrEqual(3));
    await user.click(await screen.findByTestId('remote-access-tab-install-ios'));

    const payloads = toDataURL.mock.calls.map(c => c[0]);
    const iosPayload = payloads.find(
      (p): p is string =>
        typeof p === 'string' && p.startsWith('data:application/x-apple-aspen-config'),
    );
    expect(iosPayload).toBe(
      'data:application/x-apple-aspen-config;base64,QUJDREVGRw==',
    );
  });

  it('encodes the connect QR payload with the realized bind (unchanged behavior)', async () => {
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await waitFor(() => expect(toDataURL.mock.calls.length).toBeGreaterThanOrEqual(3));

    // Order-agnostic find for the connect URL — same rationale as the
    // install-QR test above (Promise.allSettled means resolution order
    // is not deterministic).
    const payloads = toDataURL.mock.calls.map(c => c[0]);
    const connectPayload = payloads.find(
      (p): p is string => typeof p === 'string' && p.includes('?token='),
    );
    expect(connectPayload).toBe('https://192.168.1.10:1992/?token=root-tok');
  });

  it('matches the Android install-QR scheme to the realized bind (http when LAN exposure is plain)', async () => {
    // A plain (non-TLS) realized bind → the install-QR payload is also
    // http://. The route doesn't gate on scheme, but a phone reaching
    // the server over the wrong scheme hits the wrong port — symmetry
    // with the connect URL matters.
    mockBackend(
      status({
        tls_active: false,
        exposed_interfaces: [{ address: '192.168.1.10:1992', tls: false }],
      }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await waitFor(() => expect(toDataURL.mock.calls.length).toBeGreaterThanOrEqual(3));

    const payloads = toDataURL.mock.calls.map(c => c[0]);
    const installPayload = payloads.find(
      (p): p is string =>
        typeof p === 'string' && p.includes('/install-cert.der'),
    );
    expect(installPayload).toBe('http://192.168.1.10:1992/install-cert.der');
  });

  it('hides all tabs and all QRs when LAN exposure is not realized', async () => {
    // `reachable: false` ⇒ the effect bails before QR generation, so
    // neither QR is rendered and the tab bar is also absent. The
    // warning UI takes over.
    mockBackend(status({ tls_active: false, exposed_interfaces: [] }));
    render(<RemoteAccessModal onClose={() => {}} />);

    await screen.findByTestId('remote-access-warning');
    expect(screen.queryByTestId('remote-access-install-android-qr')).toBeNull();
    expect(screen.queryByTestId('remote-access-install-ios-qr')).toBeNull();
    expect(screen.queryByTestId('remote-access-connect-qr')).toBeNull();
    expect(screen.queryByTestId('remote-access-tabs')).toBeNull();
  });

  it('hides the Android install tab when the Android install-QR render fails (connect + iOS still work)', async () => {
    // The three QR generations run in parallel via Promise.allSettled —
    // a failure on one must not undo the others. Simulate the Android
    // install-QR rejecting (e.g. future renderer bug) by having it
    // throw on the second call; the connect QR (first call) and the
    // iOS QR (third call) must still render, and the Android install
    // tab itself must be absent so the user is never offered a tab
    // that would render nothing.
    toDataURL.mockImplementationOnce(async () => 'data:image/png;base64,connect');
    toDataURL.mockImplementationOnce(async () => {
      throw new Error('android install QR capacity overflow');
    });
    toDataURL.mockImplementationOnce(async () => 'data:image/png;base64,ios');
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await waitFor(() =>
      expect(screen.getByTestId('remote-access-connect-qr')).toBeTruthy(),
    );
    // Android install tab and QR both hidden — silent failure path
    // mirrors the Re-install section's "the install URL is already a
    // copy-link fallback" rationale.
    expect(screen.queryByTestId('remote-access-install-android-qr')).toBeNull();
    expect(screen.queryByTestId('remote-access-tab-install-android')).toBeNull();
    // Connect + iOS tabs both still present.
    expect(screen.getByTestId('remote-access-tab-connect')).toBeTruthy();
    expect(screen.getByTestId('remote-access-tab-install-ios')).toBeTruthy();
  });

  it('hides the iOS install tab when the iOS mobileconfig fetch fails (Android still works)', async () => {
    // Sibling to the Android-failure test above (issue #713). A
    // pre-#713 install with no `ca.key.der` on disk rejects
    // `get_root_cert_mobileconfig`; the tab must hide but the Android
    // path keeps working so the user still has a remediation route.
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
      { mobileconfig: null },
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await waitFor(() =>
      expect(screen.getByTestId('remote-access-connect-qr')).toBeTruthy(),
    );
    // iOS tab + QR hidden; Android tab + QR still present (the QR
    // generation for the Android install URL doesn't depend on the iOS
    // mobileconfig fetch, so it succeeded).
    expect(screen.queryByTestId('remote-access-tab-install-ios')).toBeNull();
    expect(screen.queryByTestId('remote-access-install-ios-qr')).toBeNull();
    expect(screen.getByTestId('remote-access-tab-install-android')).toBeTruthy();
  });

  it('hides both install tabs when both install-QR renders fail (connect still works)', async () => {
    // Three-way independent failure path: connect succeeds, both
    // install QRs reject. The Connect tab must remain; both install
    // tabs must hide. This is the worst-case "user can connect with a
    // previously-installed root CA but can't install on a fresh phone"
    // scenario — they fall back to the Re-install section's manual
    // instructions below.
    toDataURL.mockImplementationOnce(async () => 'data:image/png;base64,connect');
    toDataURL.mockImplementationOnce(async () => {
      throw new Error('android install QR capacity overflow');
    });
    toDataURL.mockImplementationOnce(async () => {
      throw new Error('ios install QR capacity overflow');
    });
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await waitFor(() =>
      expect(screen.getByTestId('remote-access-connect-qr')).toBeTruthy(),
    );
    expect(screen.queryByTestId('remote-access-tab-install-android')).toBeNull();
    expect(screen.queryByTestId('remote-access-tab-install-ios')).toBeNull();
    expect(screen.getByTestId('remote-access-tab-connect')).toBeTruthy();
  });

  it('opens the install URL in the host browser via openUrl when the fallback link is clicked', async () => {
    // The Install cert QR is the primary phone path. The desktop
    // fallback link (below the tabbed QR) opens the same URL in the
    // host browser for users who want to install on the desktop itself
    // (fresh build, cert rotated, etc.). It MUST go through `openUrl()` —
    // a raw `window.location.href` would navigate the Tauri WebView itself,
    // replacing the whole app with the cert page and no way back (issue
    // #810). The URL MUST match the realized bind — a `tauri://` or loopback
    // URL would error with ERR_INVALID_URL on a real install attempt.
    const user = userEvent.setup();
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );
    render(<RemoteAccessModal onClose={() => {}} />);

    await user.click(await screen.findByTestId('remote-access-install-link'));

    await waitFor(() =>
      expect(openUrl).toHaveBeenCalledWith('https://192.168.1.10:1992/install-cert.der'),
    );
  });

  // --- issue #1251: setState-after-unmount hazards -------------------------
  // The modal opens with a 4-IPC `Promise.all` followed by 3 parallel QR
  // generations. If the user closes the modal before any of those
  // promises resolve, the setStates inside `init()` land on an
  // unmounted component. React 19 silently drops the setStates, but the
  // closures (`getRootToken`, `getNetworkStatus`, `QRCode.toDataURL`, …)
  // keep running, holding React internals alive and producing CPU/GCP
  // churn for no visible reason. The fix is `useAsyncEffect` with
  // `signal.aborted` guards after each await + a `useRef`-stored
  // setTimeout handle that's cleared on unmount.
  //
  // These two tests pin the contract:
  // 1) Unmount during the init IPC chain → no QR generation runs (the
  //    abort happens at the `Promise.all` boundary, before any
  //    `QRCode.toDataURL` call is scheduled). The spy-on-IPC pattern
  //    below proves it without needing to introspect React internals.
  // 2) Unmount during the 2s "Copied!" feedback window → no late
  //    setState fires. The console.error spy is defense-in-depth
  //    (React 19 currently doesn't warn, but a future React could
  //    re-introduce the warning).

  it('aborts the init effect when the modal unmounts before the IPC chain resolves', async () => {
    // Defer `get_root_token` (the first IPC in the `Promise.all`) so the
    // modal cannot reach the `setHost` / `setQrDataUrl` setStates until
    // we explicitly resolve it. We unmount FIRST, then resolve — this is
    // the exact user flow the issue is filed against (open, immediately
    // close, IPC resolves milliseconds later). The fix's
    // `if (signal.aborted) return;` after `await Promise.all([...])` is
    // the load-bearing guard.
    let resolveRootToken!: (v: string) => void;
    const rootTokenPromise = new Promise<string>(resolve => {
      resolveRootToken = resolve;
    });
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_root_token':
          return rootTokenPromise;
        case 'get_local_ip':
          return Promise.resolve('192.168.1.10');
        case 'get_network_status':
          return Promise.resolve(
            status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
          );
        case 'get_cert_chain_status':
          return Promise.resolve(SAMPLE_CERT);
        case 'get_root_cert_mobileconfig':
          return Promise.resolve('QUJDREVGRw==');
        default:
          return Promise.resolve({});
      }
    });

    const { unmount } = render(<RemoteAccessModal onClose={() => {}} />);

    // Unmount before any of the deferred IPCs resolve. After this point
    // every setState inside the init() closure is a setState-on-unmounted.
    unmount();

    // Now let the deferred resolve. In the buggy code, this triggers
    // the post-`Promise.all` setStates (`setHost`, `setUnreachable`,
    // `setCertStatus`, `setInstallUrl`) and starts the QR-generation
    // chain. With the fix, the `signal.aborted` guard short-circuits
    // before any of that — `QRCode.toDataURL` is never called.
    resolveRootToken('root-tok');

    // Drain microtasks + the next macrotask so the `Promise.all` and
    // its `.then` continuations have a chance to run.
    await new Promise(resolve => setTimeout(resolve, 0));

    // The QR generator was never invoked. (In the buggy code this is
    // called once per QR × 3; in the fixed code the abort short-circuits
    // the Promise.all continuation so the allSettled QR chain never
    // starts.)
    expect(toDataURL).not.toHaveBeenCalled();
  });

  it('clears the copy-feedback timer on unmount (no late setState fires)', async () => {
    // The "Copy" button flips `certPathCopied` to true and arms a 2s
    // setTimeout to flip it back. With the buggy code, the handle isn't
    // stored so the unmount can't cancel it — the timer fires 2s later
    // and calls `setCertPathCopied(false)` on an unmounted component.
    // The fix stores the handle in a ref and clears it in a `useEffect`
    // cleanup.
    const user = userEvent.setup();
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );

    // Defense-in-depth spy: React 19 doesn't warn about setState on
    // unmounted, but a future React may re-introduce the warning. If
    // anything logs during the post-unmount window we want the test to
    // catch it. We use `mockImplementation(() => {})` so the test
    // doesn't spam vitest output if the spy IS called.
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    // Direct verification: the unmount cleanup must call clearTimeout
    // with the handle that the Copy click scheduled. Without the fix,
    // the handle is never stored and clearTimeout is never called by
    // our cleanup (React itself doesn't auto-clear timers). The "delta
    // of clearTimeout calls across unmount" assertion is robust to
    // other code paths that may legitimately call clearTimeout (e.g.
    // userEvent's internal timers).
    const clearTimeoutSpy = vi.spyOn(globalThis, 'clearTimeout');

    const { unmount } = render(<RemoteAccessModal onClose={() => {}} />);
    await user.click(await screen.findByTestId('remote-access-cert-reinstall-toggle'));
    await user.click(await screen.findByTestId('remote-access-cert-copy'));

    const clearCallsBeforeUnmount = clearTimeoutSpy.mock.calls.length;

    // Close the modal inside the 2s feedback window. In the buggy
    // code, the setTimeout callback is uncancellable.
    unmount();

    const clearCallsAfterUnmount = clearTimeoutSpy.mock.calls.length;
    // The fix's `useEffect(() => () => clearTimeout(ref.current), [])`
    // cleanup runs during unmount and adds exactly one clearTimeout
    // call for the handle we just scheduled. The buggy code adds zero.
    expect(clearCallsAfterUnmount).toBe(clearCallsBeforeUnmount + 1);

    // Wait past the 2s timeout so any uncancelled timer has fired.
    // (Real timers, not fake — the IPC chain and clipboard.writeText
    // are real Promises; only the late-firing setTimeout is what we
    // need to drain here.)
    await new Promise(resolve => setTimeout(resolve, 2100));

    expect(errorSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    clearTimeoutSpy.mockRestore();
  });
});
