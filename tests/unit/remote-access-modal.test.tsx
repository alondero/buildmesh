import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
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
  // Issue #1527: root_generation is the banner-gating counter. The
  // sample starts at 1 so existing tests don't need to opt in, and
  // reset-flow tests can build a "post-reset" cert with a higher value.
  root_generation: 1,
  cert_path: 'C:\\Users\\alond\\AppData\\Roaming\\com.alond.buildmesh\\tls\\ca.der',
};

/** A "post-reset" cert — same shape as `SAMPLE_CERT` but every value
 *  bumped so the test can prove the modal actually re-fetched the
 *  chain from the server instead of shallow-spreading (issue #1527 PR
 *  review: stale-fingerprint bug). The generation counter is `2` so
 *  it's strictly greater than `SAMPLE_CERT.root_generation` and the
 *  banner renders. */
const POST_RESET_CERT: CertChainStatus = {
  ...SAMPLE_CERT,
  root_fingerprint_sha256:
    'FF:EE:DD:CC:BB:AA:99:88:77:66:55:44:33:22:11:00:FF:EE:DD:CC:BB:AA:99:88:77:66:55:44:33:22:11:00',
  leaf_fingerprint_sha256:
    '00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF',
  root_generation: 2,
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
    // Wipe the persisted ack-generation between tests — the new banner
    // tests depend on a clean slate so a stale value from a previous
    // run can't accidentally surface or suppress the banner (issue
    // #1527).
    localStorage.removeItem('buildmesh.ackedRootGeneration');
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
        // Issue #1527: explicit Reset button IPC. The default mock
        // resolves with the cert's current generation + 1 — tests that
        // want a specific return value pass it via `invokeOnce` below.
        case 'reset_trusted_certificates':
          return Promise.resolve((cert?.root_generation ?? 0) + 1);
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
    render(<RemoteAccessModal onClose={() => {}} />);

    await screen.findByTestId('remote-access-connect-qr');
    // Issue #1292: the modal is portaled to document.body now, so the
    // <img> lives in document.body, not in the render container. Query
    // the document directly so the assertion still targets the same
    // element after the portal move.
    const img = document.body.querySelector(
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

  // --- issue #1527: explicit Reset flow ----------------------------------
  // The reset button mints a fresh root + leaf server-side and returns
  // the new generation. The frontend must (a) re-fetch the certStatus
  // to surface the new fingerprints, (b) re-mint the iOS install-QR
  // (the old one is signed by the wiped key and would silently fail to
  // install on the phone), and (c) surface the "trusted root was reset"
  // banner without immediately hiding it again. The previous version
  // of `handleResetCertificates` shallow-spread the certStatus (leaving
  // stale fingerprints), skipped the iOS QR re-mint, AND updated
  // `ackedRootGeneration` to the new generation — killing the banner
  // for the very user who triggered the rotation.
  //
  // Three tests pin the contract below:
  //   1. Reset click → IPC fires + certStatus refreshed + banner visible
  //   2. Banner persists across modal opens when localStorage has an
  //      older generation than the server's
  //   3. "Got it" dismisses the banner AND writes the new generation
  //      to localStorage so it stays hidden across subsequent opens

  it('reset click fires IPC, refreshes certStatus + iOS QR, and surfaces the banner', async () => {
    const user = userEvent.setup();
    // jsdom doesn't implement `window.confirm` — stub it so the
    // destructive-action gate in `handleResetCertificates` lets the
    // IPC chain through. We return `true` for the same reason the
    // real modal does: the test wants to exercise the post-confirm
    // reset flow. A separate `it` (or test parameter) can stub
    // `confirm` to `false` to assert the gate's blocking behavior.
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    // Override the IPC chain entirely — we need per-call tracking that
    // the default `mockBackend` helper doesn't expose. The shape
    // mirrors `mockBackend`'s defaults for the un-tracked commands.
    let certStatusCalls = 0;
    let mobileconfigCalls = 0;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_root_token':
          return Promise.resolve('root-tok');
        case 'get_local_ip':
          return Promise.resolve('192.168.1.10');
        case 'get_network_status':
          return Promise.resolve(
            status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
          );
        case 'get_cert_chain_status':
          certStatusCalls++;
          return Promise.resolve(certStatusCalls === 2 ? POST_RESET_CERT : SAMPLE_CERT);
        case 'get_root_cert_mobileconfig':
          mobileconfigCalls++;
          return Promise.resolve(mobileconfigCalls === 2 ? 'UkVTVEVSVEVE' : 'QUJDREVGRw==');
        case 'reset_trusted_certificates':
          return Promise.resolve(2);
        default:
          return Promise.resolve({});
      }
    });

    render(<RemoteAccessModal onClose={() => {}} />);

    // Sanity: initial load returns SAMPLE_CERT (gen 1) and no banner.
    await screen.findByTestId('remote-access-cert-fingerprint');
    expect(screen.queryByTestId('remote-access-root-rotated-banner')).toBeNull();

    // Confirm the reset IPC and the auto-confirmed dialog. jsdom
    // defaults `window.confirm` to true; no spy needed.
    const resetButton = await screen.findByTestId('remote-access-cert-reset');
    await user.click(resetButton);

    // After the reset promise chain resolves:
    //   - get_cert_chain_status fired a SECOND time and returned POST_RESET_CERT
    //   - get_root_cert_mobileconfig fired a SECOND time (iOS QR re-mint)
    //   - the banner is visible (ackedRootGeneration lags the new gen)
    await waitFor(() =>
      expect(screen.getByTestId('remote-access-root-rotated-banner')).toBeTruthy(),
    );
    expect(certStatusCalls).toBeGreaterThanOrEqual(2);
    expect(mobileconfigCalls).toBeGreaterThanOrEqual(2);

    // The banner and the qr section share a render tree — only one
    // mounts at a time (banner takes priority while `ackedRootGeneration`
    // lags the server). To verify the post-reset fingerprint is in
    // state, dismiss the banner first by tapping "Got it" (which writes
    // the new generation to localStorage), then assert on the freshly-
    // unmasked fingerprint element. The previous shallow-spread
    // `{ ...prev, root_generation: newGen }` would have left
    // `root_fingerprint_sha256` pointing to the deleted root's hash —
    // this assertion catches that.
    await user.click(screen.getByTestId('remote-access-root-rotated-ack'));
    const fp = await screen.findByTestId('remote-access-cert-fingerprint');
    expect(fp.textContent).toBe(POST_RESET_CERT.root_fingerprint_sha256);
    // And the iOS QR was re-generated with the post-reset mobileconfig
    // payload. The init fetch encoded `QUJDREVGRw==`; the re-mint
    // encodes `UkVTVEVSVEVE`. The exact order of `QRCode.toDataURL`
    // calls is non-deterministic (parallel Promise.allSettled), so we
    // assert on the contents of `mock.calls` rather than which call was
    // last.
    const payloads = toDataURL.mock.calls.map(c => c[0]);
    expect(
      payloads.some(
        (p): p is string =>
          typeof p === 'string' && p.startsWith('data:application/x-apple-aspen-config;base64,UkVTVEVSVEVE'),
      ),
    ).toBe(true);

    confirmSpy.mockRestore();
  });

  it('surfaces the banner on every open when persisted acked generation lags the server', async () => {
    // The banner must persist across modal closes: a user who resets,
    // closes the modal, and reopens it days later should still see the
    // re-install prompt until they tap "Got it". The previous in-
    // component state reset to `null` on every mount (issue #1527 PR
    // review: dead-banner bug). Persisted `localStorage` value here:
    // the user last acknowledged generation `0`, the server is now on
    // generation `1`, so the banner must render.
    localStorage.setItem('buildmesh.ackedRootGeneration', '0');
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );

    render(<RemoteAccessModal onClose={() => {}} />);

    const banner = await screen.findByTestId('remote-access-root-rotated-banner');
    expect(banner).toBeTruthy();
    // And the "Got it" button is present — that's how the user dismisses
    // the banner without re-resetting.
    expect(within(banner).getByTestId('remote-access-root-rotated-ack')).toBeTruthy();
  });

  it('does not surface the banner when persisted acked generation matches the server', async () => {
    // The inverse: if localStorage says "I last saw generation 1" and
    // the server is on generation 1, the banner must NOT render. This
    // is the steady state after the user taps "Got it" once.
    localStorage.setItem('buildmesh.ackedRootGeneration', '1');
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );

    render(<RemoteAccessModal onClose={() => {}} />);

    // Wait for the cert status to load (proves the modal is fully
    // initialised) — then assert the banner is absent. Using
    // `findByTestId` for the fingerprint is the canonical "modal is
    // ready" signal; the absence of the banner is then meaningful, not
    // a race against the init effect.
    await screen.findByTestId('remote-access-cert-fingerprint');
    expect(screen.queryByTestId('remote-access-root-rotated-banner')).toBeNull();
  });

  it('"Got it" dismisses the banner and persists the new generation to localStorage', async () => {
    const user = userEvent.setup();
    localStorage.setItem('buildmesh.ackedRootGeneration', '0');
    mockBackend(
      status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
    );

    render(<RemoteAccessModal onClose={() => {}} />);

    const banner = await screen.findByTestId('remote-access-root-rotated-banner');
    await user.click(within(banner).getByTestId('remote-access-root-rotated-ack'));

    // The banner disappears immediately. We can't assert on its absence
    // (React 19 would silently keep it until next render), so we wait
    // for the queryByTestId to flip from "found" to "null".
    await waitFor(() =>
      expect(screen.queryByTestId('remote-access-root-rotated-banner')).toBeNull(),
    );
    // And the persisted generation is now 1 (the server's current
    // value), so a future modal open won't re-render the banner.
    expect(localStorage.getItem('buildmesh.ackedRootGeneration')).toBe('1');
  });

  // --- issue #1527: iOS QR regeneration failure must not nuke the modal.
  // Round-2 review flagged that the reset path's `setError(formatError(e))`
  // for an iOS-mobileconfig failure unmounts the entire modal (the
  // `error` branch replaces tabs + connect QR + Android install QR +
  // fingerprint + reset button with a single error string). The right
  // contract mirrors the init() path's silent-failure: clear the iOS
  // QR so the tab hides, console.warn for diagnosis, and leave every
  // other affordance functional. This test pins that contract.
  it('reset click with iOS mobileconfig failure keeps the modal functional (no fatal setError)', async () => {
    const user = userEvent.setup();
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    let mobileconfigCalls = 0;
    let certStatusCalls = 0;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_root_token':
          return Promise.resolve('root-tok');
        case 'get_local_ip':
          return Promise.resolve('192.168.1.10');
        case 'get_network_status':
          return Promise.resolve(
            status({ exposed_interfaces: [{ address: '192.168.1.10:1992', tls: true }] }),
          );
        case 'get_cert_chain_status':
          certStatusCalls++;
          // Init returns gen 1; post-reset re-fetch returns gen 2 so
          // the banner condition (`2 > 1`) actually triggers after
          // the reset. A no-op reset would not exercise the contract
          // we're testing (the banner wouldn't render in the first
          // place).
          return Promise.resolve(certStatusCalls === 2 ? POST_RESET_CERT : SAMPLE_CERT);
        case 'get_root_cert_mobileconfig':
          mobileconfigCalls++;
          // Init fetch succeeds; the post-reset re-mint rejects. The
          // reset path must catch and gracefully hide the iOS tab.
          return mobileconfigCalls === 2
            ? Promise.reject(new Error('PKCS#7 CMS sign failed'))
            : Promise.resolve('QUJDREVGRw==');
        case 'reset_trusted_certificates':
          return Promise.resolve(2);
        default:
          return Promise.resolve({});
      }
    });

    render(<RemoteAccessModal onClose={() => {}} />);
    await screen.findByTestId('remote-access-cert-fingerprint');

    await user.click(screen.getByTestId('remote-access-cert-reset'));

    // The banner must still surface — the cert was reset, the modal
    // is functional. If `setError` had been called, the entire modal
    // body would be unmounted and the banner wouldn't exist.
    await waitFor(() =>
      expect(screen.getByTestId('remote-access-root-rotated-banner')).toBeTruthy(),
    );
    // The fingerprint must still be readable (the cert-status fetch
    // succeeded); dismiss the banner to verify.
    await user.click(screen.getByTestId('remote-access-root-rotated-ack'));
    const fp = await screen.findByTestId('remote-access-cert-fingerprint');
    expect(fp.textContent).toBe(POST_RESET_CERT.root_fingerprint_sha256);
    // The connect-QR is still rendered.
    expect(screen.getByTestId('remote-access-connect-qr')).toBeTruthy();
    // console.warn fired (not console.error — using setError would
    // have routed through the React error boundary and logged here).
    expect(warnSpy).toHaveBeenCalled();
    // Banner dismiss wrote the new generation to localStorage; a
    // future modal open won't re-render it.
    expect(localStorage.getItem('buildmesh.ackedRootGeneration')).toBe('2');

    confirmSpy.mockRestore();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
  });
});
