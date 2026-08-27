import { formatError } from '../../lib/errorUtils';
import { useState, useEffect, useRef } from 'react';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import QRCode from 'qrcode';
import { openUrl } from '@tauri-apps/plugin-opener';
import * as api from '../../lib/tauri';
import { Modal, ModalCloseButton } from '../shared/Modal';
import type { NetworkStatus } from '../../types/generated/NetworkStatus';
import type { CertChainStatus } from '../../types/generated/CertChainStatus';

interface RemoteAccessModalProps {
  onClose: () => void;
}

/**
 * Build the URL the phone should hit from the server's *realized* network
 * exposure, not a hardcoded scheme/port. When LAN exposure is enabled the
 * non-loopback interfaces serve self-signed TLS (issue #501), so the QR must
 * say `https://` — a phone sending plain HTTP to a TLS listener gets the
 * browser's "sent an invalid response" dead-end. We match the scheme to the
 * realized bind (`tls`) and take both host and port from its `address`
 * (`ip:port`), which already accounts for the 1992→1994 fallback.
 *
 * Falls back to the discovered IP + reported port over plain HTTP only when no
 * non-loopback listener is bound (exposure off / loopback only); `reachable`
 * is false in that case so the modal warns instead of handing out a dead URL.
 *
 * IPv4 wins over IPv6 regardless of enumeration order: a bracketed IPv6 address
 * (`[…]:port`) in the QR makes the phone's browser bail with
 * ERR_INVALID_ARGUMENT, and IPv4 LAN is the canonical "phone on the same
 * Wi-Fi" target. We only hand out an IPv6 URL when no IPv4 interface is bound.
 */
export function buildRemoteAccessUrl(
  status: NetworkStatus,
  fallbackIp: string,
  rootToken: string,
): { url: string; host: string; reachable: boolean } {
  // A realized IPv6 bind renders bracketed (`[::1]:port`); IPv4 never does.
  const isIpv4 = (b: { address: string }) => !b.address.includes('[');
  // Prefer TLS over plain (HTTPS is what an exposed interface serves) and IPv4
  // over IPv6 (reachable from the phone), in that priority order. Falling all
  // the way through to any realized bind keeps an unexpected listener usable
  // with the correct scheme rather than a mismatched one.
  const ifaces = status.exposed_interfaces;
  const bind =
    ifaces.find(b => b.tls && isIpv4(b)) ??
    ifaces.find(b => b.tls) ??
    ifaces.find(isIpv4) ??
    ifaces[0];
  if (bind) {
    const scheme = bind.tls ? 'https' : 'http';
    return {
      url: `${scheme}://${bind.address}/?token=${rootToken}`,
      host: bind.address,
      reachable: true,
    };
  }
  const host = `${fallbackIp}:${status.port}`;
  return {
    url: `http://${host}/?token=${rootToken}`,
    host,
    reachable: false,
  };
}

export function RemoteAccessModal({ onClose }: RemoteAccessModalProps) {
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [host, setHost] = useState<string>('discovering...');
  const [error, setError] = useState<string | null>(null);
  const [unreachable, setUnreachable] = useState(false);
  // Server's current root CA fingerprint (issue #635). Shown below the QR so
  // a user whose installed root is stale can compare before re-installing.
  // Fetch failure is silent — the modal still works without it.
  const [certStatus, setCertStatus] = useState<CertChainStatus | null>(null);
  // The rendered QR PNG of the install-cert data: URL (issue #702). The
  // data: URL itself stays as a local const inside the effect — it's
  // the *QR payload*, not a render artifact, so it doesn't need state.
  // The PNG IS the visible image: browsers won't render a
  // `data:application/x-x509-ca-cert` URL as an image, so we use
  // QRCode.toDataURL to produce a viewable PNG of the data: URL.
  const [installQrDataUrl, setInstallQrDataUrl] = useState<string | null>(null);
  // The rendered QR PNG of the iOS `.mobileconfig` data: URL (issue #713).
  // Sibling to `installQrDataUrl` — kept separate so a failure on either
  // path is independently hideable. iOS Safari does NOT intercept the
  // Android path's HTTPS URL or the `application/x-x509-ca-cert` data:
  // MIME; it needs an Apple Configurator 2 `.mobileconfig` profile signed
  // with the root CA's private key, which `getRootCertMobileconfig` builds
  // and base64-encodes for embedding in this QR.
  const [installIosQrDataUrl, setInstallIosQrDataUrl] = useState<string | null>(null);
  // HTTPS URL of `/install-cert.der` for the manual install fallback
  // (issue #702 review finding). The Re-install section surfaces this
  // so a user whose phone can't scan the install-QR (older Android,
  // custom camera apps, ad-blockers) can open the URL in their phone's
  // browser and let Chrome's MIME-routed install take over.
  const [installUrl, setInstallUrl] = useState<string | null>(null);
  // Inline sub-section toggle for the "Re-install root CA" affordance.
  const [showReinstall, setShowReinstall] = useState(false);
  // "Copied!" feedback for the cert path copy button. Mirrors the
  // AppSettingsModal.tsx:670 clipboard pattern (2s timeout).
  const [certPathCopied, setCertPathCopied] = useState(false);
  // Issue #1251: the auto-clear setTimeout must be cancellable from
  // the unmount cleanup, otherwise a close inside the 2s feedback
  // window lands `setCertPathCopied(false)` on an unmounted component.
  // `useRef` keeps the handle stable across renders without triggering
  // a re-render when we mutate `.current`. Mirrors the
  // `successTimerRef` pattern in `WorktreeManagerTab.tsx:367-374`.
  const copyFeedbackTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 'connect' is the default — modal opens for the common case. Tabs
  // let each QR render at full modal width so a phone can scan from
  // across the room; a side-by-side layout made both QRs too small.
  // Issue #713: a third tab was added for the iOS-specific `.mobileconfig`
  // install path; same full-width rationale (each tab → one QR → 384px).
  const [activeTab, setActiveTab] = useState<'connect' | 'install-android' | 'install-ios'>(
    'connect',
  );

  useAsyncEffect((signal) => {
    const init = async () => {
      try {
        // These reads are independent — dispatch them together rather than
        // serially so the modal opens in one round-trip. The `getLocalIp`
        // fallback string is a UX placeholder, not a typed contract: it is only
        // used when no LAN listener is bound, in which case `reachable` is false
        // and we warn rather than rely on it. `getCertChainStatus` failure is
        // silently swallowed (`.catch(() => null)`) — the modal still works,
        // we just won't show the fingerprint.
        const [rootToken, status, localIp, cert] = await Promise.all([
          api.getRootToken(),
          api.getNetworkStatus(),
          api.getLocalIp().catch(() => '192.168.1.x'),
          api.getCertChainStatus().catch(() => null),
        ]);
        // Issue #1251: a fast close (user opens + immediately dismisses
        // the modal) aborts the signal before any of the IPC promises
        // resolve. Skip every setState below — the component is gone
        // and React 19 would silently drop them, but the closures
        // (`buildRemoteAccessUrl`, `QRCode.toDataURL`, …) would still
        // run, holding React internals + IPC mocks alive for nothing.
        if (signal.aborted) return;

        const { url, host: displayHost, reachable } = buildRemoteAccessUrl(
          status,
          localIp,
          rootToken,
        );
        setHost(displayHost);
        setUnreachable(!reachable);
        setCertStatus(cert);
        // Hoist the install URL once — the QR payload and the fallback
        // link must stay in sync, and `${origin}/install-cert.der` has
        // future query-string / path churn risk (#702 rotation).
        const installUrlValue = reachable ? `${new URL(url).origin}/install-cert.der` : null;
        setInstallUrl(installUrlValue);

        if (!reachable) {
          // Skip all QRs when the LAN isn't actually exposed — the
          // warning UI below takes over. Generating a QR for a
          // `reachable: false` URL would hand the user a dead link.
          return;
        }

        // Three QRs are independent. Run them in parallel so React 19
        // auto-batches the setStates. `Promise.allSettled` so any
        // install-QR failure cannot undo the connect-QR's successful
        // setState — the install URL is already exposed as a fallback
        // link below, and each install tab is gated on its own QR
        // presence so a failure on one platform hides only that tab.
        //
        // `installUrlValue!` is sound: the early-return above guarantees
        // non-null whenever this line is reached.
        //
        // Issue #713: `getRootCertMobileconfig` is awaited alongside
        // the other two — it does the PKCS#7/CMS sign server-side, which
        // is ~5–15 ms on a fresh root. Failure here (e.g. pre-#713 install
        // with no `ca.key.der`) is silently swallowed and the iOS tab is
        // hidden, matching the existing Android tab's failure semantics.
        const [connectResult, installResult, installIosResult] = await Promise.allSettled([
          QRCode.toDataURL(url, {
            width: 384,
            margin: 2,
            color: { dark: '#e0e0e0', light: '#1a1a1a' },
          }),
          // Encode at the SAME pixel size as the render box (w-96 = 384px).
          // A 256px raster upscaled by the browser blurs the QR modules
          // and degrades scan reliability at distance — the whole point
          // of the tabs layout.
          QRCode.toDataURL(installUrlValue!, {
            width: 384,
            margin: 2,
            color: { dark: '#e0e0e0', light: '#1a1a1a' },
          }),
          // iOS `.mobileconfig` QR (issue #713). Payload is a
          // `data:application/x-apple-aspen-config;base64,…` URL —
          // built inline because the base64 string lives only here
          // (it's the QR's content, not a separate render artifact).
          api
            .getRootCertMobileconfig()
            .then(b64 => `data:application/x-apple-aspen-config;base64,${b64}`)
            .then(payload =>
              QRCode.toDataURL(payload, {
                width: 384,
                margin: 2,
                color: { dark: '#e0e0e0', light: '#1a1a1a' },
              }),
            ),
        ]);
        // Issue #1251: the QR-generation chain runs to completion
        // regardless (the `qrcode` library has no abort handle), so
        // the modal may have unmounted during the wait. Drop the
        // results instead of setState'ing on a dead component.
        if (signal.aborted) return;
        if (connectResult.status === 'fulfilled') {
          setQrDataUrl(connectResult.value);
        } else {
          // Only the connect-QR failure is fatal — without it the user
          // has no way to reach the server.
          throw connectResult.reason;
        }
        if (installResult.status === 'fulfilled') {
          setInstallQrDataUrl(installResult.value);
        }
        // Install-QR failure is silent — the install URL is exposed as
        // a copy-link fallback, so the user has a working remediation
        // path even when the QR fails to render.
        if (installIosResult.status === 'fulfilled') {
          setInstallIosQrDataUrl(installIosResult.value);
        }
        // Same silent-failure contract as the Android install-QR: the
        // iOS tab is gated on `installIosQrDataUrl` presence, so a
        // rejection here just hides it — the user keeps the connect
        // and Android paths.
      } catch (e) {
        // Issue #1251: don't setError on a dead component — the modal
        // closed mid-fetch and the user already moved on.
        if (signal.aborted) return;
        setError(formatError(e));
      }
    };
    init();
  }, []);

  // Issue #1251: clear the copy-feedback timer on unmount so a late
  // `setCertPathCopied(false)` cannot land on an unmounted tree (e.g.
  // user copies the cert path, then closes the modal within 2s).
  // Mirror the `WorktreeManagerTab.tsx:367-374` pattern: a dedicated
  // effect with an empty dep list and the cleanup in the return.
  useEffect(() => {
    return () => {
      if (copyFeedbackTimerRef.current !== null) {
        clearTimeout(copyFeedbackTimerRef.current);
        copyFeedbackTimerRef.current = null;
      }
    };
  }, []);

  const handleCopyCertPath = async () => {
    // Narrow `cert_path` against the optional TS field (skipped in the HTTP
    // route's JSON via `#[serde(skip_serializing_if = "Option::is_none")]`).
    // In practice the desktop modal only ever reads the Tauri command
    // response, which always sets `cert_path: Some(...)`; the optional shape
    // is so a future HTTP consumer of the same TS type can read it safely.
    const path = certStatus?.cert_path;
    if (!path) return;
    try {
      await navigator.clipboard.writeText(path);
      setCertPathCopied(true);
      // Issue #1251: stash the handle in a ref so the unmount cleanup
      // can cancel it. The previous bare `setTimeout` had no way to be
      // cleared — closing the modal inside the 2s feedback window left
      // the callback to fire `setCertPathCopied(false)` on a dead
      // component. We also clear any in-flight previous timer so a
      // second Copy click inside the 2s window restarts the countdown
      // instead of double-firing the auto-clear.
      if (copyFeedbackTimerRef.current !== null) {
        clearTimeout(copyFeedbackTimerRef.current);
      }
      copyFeedbackTimerRef.current = setTimeout(() => {
        setCertPathCopied(false);
        copyFeedbackTimerRef.current = null;
      }, 2000);
    } catch {
      // jsdom in tests + some browsers without clipboard permission reject.
      // The fingerprint text below is still copyable manually, so this is
      // non-fatal — match AppSettingsModal's pattern of letting the copy
      // surface its own error if it must.
    }
  };

  const handleInstall = () => {
    // Open the install URL in the desktop's default browser. The PRIMARY
    // phone path is the install-QR above (scan with phone camera); this
    // fallback is for the desktop user who wants to install on the host
    // itself (e.g. fresh build, cert rotated, LAN exposure toggle flipped).
    // Must go through `openUrl()` — a raw `window.location.href` navigates
    // the Tauri WebView itself, replacing the whole app with the cert-
    // download/TLS page and no way back (issue #810; the knowledge-primer
    // external-link anti-pattern).
    if (!installUrl) return;
    openUrl(installUrl).catch(console.error);
  };

  return (
    <Modal onClose={onClose} labelledBy="remote-access-title" maxWidth="max-w-lg" className="p-8">
        <div className="absolute top-5 right-5">
          <ModalCloseButton onClose={onClose} label="Close remote access" />
        </div>

        <h2 id="remote-access-title" className="text-2xl font-semibold text-text-primary mb-2">Remote Access</h2>
        <p className="text-base text-text-muted mb-6">
          Scan with your phone camera to connect.
        </p>

        {error ? (
          <div className="text-status-error text-base">{error}</div>
        ) : unreachable ? (
          <div
            data-testid="remote-access-warning"
            role="alert"
            className="text-status-error text-base"
          >
            LAN exposure is on, but no network interface is actually exposed —
            your phone can't reach this computer. Check the "Expose to LAN"
            status in Settings (TLS may have failed to start, or no LAN
            interface is available).
          </div>
        ) : qrDataUrl ? (
          <div className="flex flex-col items-center">
            {/* Tab bar. Connect is the default — the modal opens for the
                common case (phone already has the root, user wants to
                connect). Each install tab is opt-in for the fresh-phone /
                rotated-cert case and renders only when its QR is available
                (a failed Android fetch hides the Android tab; a failed iOS
                fetch hides the iOS tab). Each tab renders its QR at full
                modal width so the phone can scan from a comfortable distance. */}
            <div
              data-testid="remote-access-tabs"
              role="tablist"
              className="flex w-full mb-4 border-b border-border-subtle"
            >
              <button
                data-testid="remote-access-tab-connect"
                role="tab"
                aria-selected={activeTab === 'connect'}
                onClick={() => setActiveTab('connect')}
                className={`flex-1 px-3 py-2 text-sm font-medium ${
                  activeTab === 'connect'
                    ? 'text-accent-cyan border-b-2 border-accent-cyan'
                    : 'text-text-muted hover:text-text-secondary'
                }`}
                type="button"
              >
                Connect
              </button>
              {installQrDataUrl && (
                <button
                  data-testid="remote-access-tab-install-android"
                  role="tab"
                  aria-selected={activeTab === 'install-android'}
                  onClick={() => setActiveTab('install-android')}
                  className={`flex-1 px-3 py-2 text-sm font-medium ${
                    activeTab === 'install-android'
                      ? 'text-accent-cyan border-b-2 border-accent-cyan'
                      : 'text-text-muted hover:text-text-secondary'
                  }`}
                  type="button"
                >
                  Install — Android
                </button>
              )}
              {installIosQrDataUrl && (
                <button
                  data-testid="remote-access-tab-install-ios"
                  role="tab"
                  aria-selected={activeTab === 'install-ios'}
                  onClick={() => setActiveTab('install-ios')}
                  className={`flex-1 px-3 py-2 text-sm font-medium ${
                    activeTab === 'install-ios'
                      ? 'text-accent-cyan border-b-2 border-accent-cyan'
                      : 'text-text-muted hover:text-text-secondary'
                  }`}
                  type="button"
                >
                  Install — iOS
                </button>
              )}
            </div>
            {/* Single active QR at full size. All three tabs render the same
                w-96 h-96 size as the original connect QR — the side-by-side
                attempt squeezed both into w-40 which failed at scan
                distance. Mounting only the active QR avoids keeping the
                inactive <img> in the DOM (a phone scanning the wrong tab
                would see the wrong URL in any accidental screen-grab). */}
            {activeTab === 'connect' && (
              <div
                data-testid="remote-access-connect-qr"
                className="flex flex-col items-center"
              >
                <img
                  src={qrDataUrl}
                  alt="QR Code to connect"
                  className="w-96 h-96 rounded-md border border-border-subtle"
                />
                <div className="mt-2 text-base text-text-secondary font-medium font-mono">
                  {host}
                </div>
              </div>
            )}
            {activeTab === 'install-android' && installQrDataUrl && (
              <div
                data-testid="remote-access-install-android-qr"
                className="flex flex-col items-center"
              >
                <img
                  src={installQrDataUrl}
                  alt="QR Code to install Buildmesh root CA on Android"
                  className="w-96 h-96 rounded-md border border-border-subtle"
                />
                <div className="mt-2 text-sm text-text-muted text-center">
                  Scan with your Android phone to install the root CA.
                </div>
              </div>
            )}
            {activeTab === 'install-ios' && installIosQrDataUrl && (
              <div
                data-testid="remote-access-install-ios-qr"
                className="flex flex-col items-center"
              >
                <img
                  src={installIosQrDataUrl}
                  alt="QR Code to install Buildmesh root CA on iOS"
                  className="w-96 h-96 rounded-md border border-border-subtle"
                />
                <div className="mt-2 text-sm text-text-muted text-center">
                  Scan with your iPhone to install the profile, then enable
                  trust in Settings → General → About → Certificate Trust
                  Settings.
                </div>
              </div>
            )}
            {/* Cert status (issue #635): when the phone's installed root CA
                doesn't match the server's (regen on LAN-IP change, fresh
                `cargo build`, etc.) TLS fails silently with `CertificateUnknown`
                and the user has no signal. We always show the server's current
                fingerprint and a "Re-install" affordance so the user can act
                without reaching for `openssl`. No "mismatch detected" banner —
                we can't see the phone side from here, and a banner that almost-
                never-fires erodes trust. The fingerprint + Re-install button
                are the always-on remediation path. */}
            {installUrl && certStatus && (
              <div
                data-testid="remote-access-install-fallback"
                className="mt-4 w-full text-left"
              >
                <button
                  data-testid="remote-access-install-link"
                  onClick={handleInstall}
                  className="text-xs text-accent-cyan hover:underline"
                  type="button"
                >
                  Or open the install URL on this computer →
                </button>
              </div>
            )}
            {certStatus && (
              <div
                data-testid="remote-access-cert-status"
                className="mt-4 w-full text-left"
              >
                <div className="text-xs text-text-muted mb-1">
                  Server root CA fingerprint
                  <span className="ml-2 text-text-muted/70">
                    (or re-install manually for iOS / older Android)
                  </span>
                </div>
                <div
                  data-testid="remote-access-cert-fingerprint"
                  className="text-xs font-mono text-text-secondary break-all bg-bg-base/40 rounded-md px-2 py-1"
                >
                  {certStatus.root_fingerprint_sha256}
                </div>
                <button
                  data-testid="remote-access-cert-reinstall-toggle"
                  onClick={() => setShowReinstall(s => !s)}
                  className="mt-2 text-xs text-accent-cyan hover:underline"
                  type="button"
                >
                  {showReinstall ? 'Hide' : 'Re-install root CA'}
                </button>
                {showReinstall && (
                  <div
                    data-testid="remote-access-cert-reinstall"
                    className="mt-2 text-xs text-text-muted border border-border-subtle rounded-md p-3 space-y-2"
                  >
                    {installUrl && (
                      <div>
                        <div className="mb-1 text-text-secondary">
                          On your phone, open this URL in the browser:
                        </div>
                        <code
                          data-testid="remote-access-cert-install-url"
                          className="block font-mono break-all bg-bg-base/40 rounded-md px-2 py-1 text-text-secondary"
                        >
                          {installUrl}
                        </code>
                        <div className="mt-1 text-text-muted/80">
                          Chrome auto-routes the .der download into the
                          system cert installer — same one-tap install as
                          the QR above, but via the browser instead.
                        </div>
                      </div>
                    )}
                    <div>
                      <div className="mb-1 text-text-secondary">
                        On your computer, the cert is at:
                      </div>
                      <div className="flex items-start gap-2">
                        <code
                          data-testid="remote-access-cert-path"
                          className="flex-1 font-mono break-all bg-bg-base/40 rounded-md px-2 py-1 text-text-secondary"
                        >
                          {certStatus.cert_path ?? ''}
                        </code>
                        <button
                          data-testid="remote-access-cert-copy"
                          onClick={handleCopyCertPath}
                          className="shrink-0 px-2 py-1 rounded-md bg-border-subtle hover:bg-border-default text-text-secondary"
                          type="button"
                        >
                          {certPathCopied ? 'Copied!' : 'Copy'}
                        </button>
                      </div>
                    </div>
                    <div>
                      <div className="font-medium text-text-secondary mb-1">
                        Android
                      </div>
                      <ol className="list-decimal list-inside space-y-0.5">
                        <li>Transfer <code className="font-mono">ca.der</code> to the phone (AirDrop / email / cable).</li>
                        <li>Settings → Security → Encryption &amp; credentials → Install a certificate → CA certificate.</li>
                        <li>Pick <code className="font-mono">ca.der</code> from the path above.</li>
                      </ol>
                    </div>
                    <div>
                      <div className="font-medium text-text-secondary mb-1">
                        iOS
                      </div>
                      <ol className="list-decimal list-inside space-y-0.5">
                        <li>Scan the <span className="font-medium text-text-secondary">Install — iOS</span> QR above — Safari installs the signed <code className="font-mono">.mobileconfig</code> profile with a single tap.</li>
                        <li>Settings → General → About → Certificate Trust Settings → enable the new root. (Self-signed roots always need this one tap; signing only removes the "Not Signed" warning, it does not auto-trust.)</li>
                      </ol>
                      <div className="mt-1 text-text-muted/80">
                        If scanning fails (older iOS, custom camera apps), transfer
                        {' '}<code className="font-mono">ca.der</code> to the phone and
                        install via Settings → General → VPN &amp; Device Management.
                      </div>
                    </div>
                  </div>
                )}
              </div>
            )}
          </div>
        ) : (
          <div className="flex justify-center py-12">
            <div className="animate-spin w-8 h-8 border-2 border-accent-cyan border-t-transparent rounded-full" />
          </div>
        )}

        <div className="mt-6 pt-4 border-t border-border-subtle">
          <p className="text-sm text-text-muted">
            Make sure your phone is on the same network as this computer.
          </p>
        </div>
    </Modal>
  );
}
