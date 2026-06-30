import { useState, useEffect } from 'react';
import QRCode from 'qrcode';
import * as api from '../../lib/tauri';
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

  useEffect(() => {
    const init = async () => {
      try {
        // These reads are independent — dispatch them together rather than
        // serially so the modal opens in one round-trip. The `getLocalIp`
        // fallback string is a UX placeholder, not a typed contract: it is only
        // used when no LAN listener is bound, in which case `reachable` is false
        // and we warn rather than rely on it. `getCertChainStatus` and
        // `getRootCertDer` failures are silently swallowed (`.catch(() => null)`)
        // — the modal still works, we just won't show the install affordance.
        const [rootToken, status, localIp, cert, rootDerB64] = await Promise.all([
          api.getRootToken(),
          api.getNetworkStatus(),
          api.getLocalIp().catch(() => '192.168.1.x'),
          api.getCertChainStatus().catch(() => null),
          api.getRootCertDer().catch(() => null),
        ]);

        const { url, host: displayHost, reachable } = buildRemoteAccessUrl(
          status,
          localIp,
          rootToken,
        );
        setHost(displayHost);
        setUnreachable(!reachable);
        setCertStatus(cert);
        // Manual-install URL for the Re-install section (issue #702).
        // Same origin as the connect URL, /install-cert.der path.
        // Set even when unreachable so the user's clipboard fallback
        // works if they have a real bind reachable via a different
        // path. (Mostly unreachable ⇒ no usable URL, but the render
        // hides the QR section entirely in that case anyway.)
        if (reachable) {
          setInstallUrl(`${new URL(url).origin}/install-cert.der`);
        } else {
          setInstallUrl(null);
        }

        if (!reachable) {
          // Skip both QRs when the LAN isn't actually exposed — the
          // warning UI below takes over. Generating a QR for a
          // `reachable: false` URL would hand the user a dead link
          // (the connect URL is the discovered-IP fallback, not a
          // realized bind).
          return;
        }

        // Both QRs are independent (no data dependency between the
        // connect URL and the cert data: URL) — run them in parallel
        // so React 19 auto-batches the two setStates into one render.
        // Per-QR try/catch: a 2.7+ KB cert would throw at the qrcode
        // lib's capacity check; that must NOT undo the connect-QR's
        // successful setState, so each phase guards its own.
        const results = await Promise.allSettled([
          QRCode.toDataURL(url, {
            width: 384,
            margin: 2,
            color: { dark: '#e0e0e0', light: '#1a1a1a' },
          }),
          // Phone install QR (issue #702). Embed the root cert as a
          // `data:application/x-x509-ca-cert;base64,...` URL so the
          // phone's OS CA installer intercepts the scan — Android
          // Chrome (≥ 90) and iOS Safari (≥ 14) both route this MIME
          // into native install, bypassing the chicken-and-egg of
          // "phone needs the cert before it trusts the server, but
          // the server serves the cert over TLS". Pinned to
          // error-correction level L: a 2 KB cert just fits at L,
          // fails at the qrcode lib's default M, so future-larger
          // certs (e.g. RSA-4096 roots) get ~30% headroom for free.
          rootDerB64
            ? QRCode.toDataURL(
                `data:application/x-x509-ca-cert;base64,${rootDerB64}`,
                {
                  width: 256,
                  margin: 2,
                  errorCorrectionLevel: 'L',
                  color: { dark: '#e0e0e0', light: '#1a1a1a' },
                },
              )
            : Promise.resolve(null),
        ]);
        const [connectResult, installResult] = results;
        if (connectResult.status === 'fulfilled') {
          setQrDataUrl(connectResult.value);
        } else {
          // Only the connect-QR failure is fatal to the modal — the
          // install-QR has its own path below.
          throw connectResult.reason;
        }
        if (installResult.status === 'fulfilled' && installResult.value) {
          setInstallQrDataUrl(installResult.value);
        } else if (installResult.status === 'rejected') {
          // Install-QR failure (cert > capacity, etc.) must NOT undo
          // the connect-QR's successful setState. Swallow and continue
          // — the Re-install toggle below is the user's remediation
          // path. (issue #702 review finding: silent brick.)
        }
      } catch (e) {
        setError(String(e));
      }
    };
    init();
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
      setTimeout(() => setCertPathCopied(false), 2000);
    } catch {
      // jsdom in tests + some browsers without clipboard permission reject.
      // The fingerprint text below is still copyable manually, so this is
      // non-fatal — match AppSettingsModal's pattern of letting the copy
      // surface its own error if it must.
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" onClick={onClose}>
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/70" />

      {/* Modal */}
      <div
        className="relative bg-bg-overlay border border-border-default rounded-lg shadow-2xl p-8 max-w-lg w-full"
        onClick={e => e.stopPropagation()}
      >
        <button
          onClick={onClose}
          className="absolute top-5 right-5 text-text-muted hover:text-text-secondary text-3xl"
        >
          ×
        </button>

        <h2 className="text-2xl font-semibold text-text-primary mb-2">Remote Access</h2>
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
            {/* Install-QR first (Step 1) so the flow reads top-to-bottom:
                install the root, then connect. Issue #702. The 3-button
                picker previously at this slot navigated the DESKTOP's
                WebView, installing the cert on the PC not the phone. The
                data: URL inside this QR routes through the phone's OS CA
                installer instead — no desktop WebView involved.

                Both the install-QR and the "Step N" labels are gated on
                `installQrDataUrl` together: when the cert fetch fails or
                the cert is too large for a single QR, we drop BOTH
                step labels so the user doesn't see a "Step 2" with no
                preceding "Step 1". */}
            {installQrDataUrl && (
              <div
                data-testid="remote-access-install-qr"
                className="flex flex-col items-center mb-6"
              >
                <div className="text-xs text-text-muted mb-2">
                  Step 1 — scan with your phone to install the root CA
                </div>
                <img
                  src={installQrDataUrl}
                  alt="Install root CA QR"
                  data-testid="remote-access-install-qr-img"
                  className="w-64 h-64 rounded border border-border-subtle"
                />
              </div>
            )}
            <div className="text-xs text-text-muted mb-2">
              {installQrDataUrl ? 'Step 2 — scan to connect' : 'Scan to connect'}
            </div>
            <img
              src={qrDataUrl}
              alt="QR Code"
              className="w-96 h-96 rounded border border-border-subtle mb-4"
            />
            <div className="text-base text-text-muted font-mono text-center">
              <div>{host}</div>
            </div>
            {/* Cert status (issue #635): when the phone's installed root CA
                doesn't match the server's (regen on LAN-IP change, fresh
                `cargo build`, etc.) TLS fails silently with `CertificateUnknown`
                and the user has no signal. We always show the server's current
                fingerprint and a "Re-install" affordance so the user can act
                without reaching for `openssl`. No "mismatch detected" banner —
                we can't see the phone side from here, and a banner that almost-
                never-fires erodes trust. The fingerprint + Re-install button
                are the always-on remediation path. */}
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
                  className="text-xs font-mono text-text-secondary break-all bg-bg-base/40 rounded px-2 py-1"
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
                    className="mt-2 text-xs text-text-muted border border-border-subtle rounded p-3 space-y-2"
                  >
                    {installUrl && (
                      <div>
                        <div className="mb-1 text-text-secondary">
                          On your phone, open this URL in the browser:
                        </div>
                        <code
                          data-testid="remote-access-cert-install-url"
                          className="block font-mono break-all bg-bg-base/40 rounded px-2 py-1 text-text-secondary"
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
                          className="flex-1 font-mono break-all bg-bg-base/40 rounded px-2 py-1 text-text-secondary"
                        >
                          {certStatus.cert_path ?? ''}
                        </code>
                        <button
                          data-testid="remote-access-cert-copy"
                          onClick={handleCopyCertPath}
                          className="shrink-0 px-2 py-1 rounded bg-border-subtle hover:bg-border-default text-text-secondary"
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
                        <li>Transfer <code className="font-mono">ca.der</code> to the phone.</li>
                        <li>Settings → General → VPN &amp; Device Management → tap the profile → Install.</li>
                        <li>Settings → General → About → Certificate Trust Settings → enable the new root.</li>
                      </ol>
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
      </div>
    </div>
  );
}
