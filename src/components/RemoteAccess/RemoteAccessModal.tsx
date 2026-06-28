import { useState, useEffect } from 'react';
import QRCode from 'qrcode';
import * as api from '../../lib/tauri';
import type { NetworkStatus } from '../../types/generated/NetworkStatus';
import type { CertChainStatus } from '../../types/generated/CertChainStatus';

interface RemoteAccessModalProps {
  onClose: () => void;
}

/**
 * Devices the one-tap install supports (issue #636). iOS is deliberately
 * absent — `.mobileconfig` requires a signed XML payload and the profile
 * structure Apple Configurator publishes is a separate 1-2 day scope, filed
 * as a follow-up. Picking "this Mac" or "this PC" picks the same `/install-
 * cert.der` URL the phone does; the OS-native installer is what differs, not
 * the bytes served.
 */
export type InstallTarget = 'android' | 'windows' | 'macos';

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
  const [installUrl, setInstallUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [unreachable, setUnreachable] = useState(false);
  // Server's current root CA fingerprint (issue #635). Shown below the QR so
  // a user whose installed root is stale can compare before re-installing.
  // Fetch failure is silent — the modal still works without it.
  const [certStatus, setCertStatus] = useState<CertChainStatus | null>(null);
  // Inline sub-section toggle for the "Re-install root CA" affordance.
  const [showReinstall, setShowReinstall] = useState(false);
  // "Copied!" feedback for the cert path copy button. Mirrors the
  // AppSettingsModal.tsx:670 clipboard pattern (2s timeout).
  const [certPathCopied, setCertPathCopied] = useState(false);
  // Target-device picker for the one-tap install (issue #636). The desktop
  // modal is NOT the device that needs the cert — `navigator.userAgent` here
  // reflects the host (Windows/macOS), not the phone. The user picks the
  // device they're setting up; collapsed by default so the modal stays calm
  // for users who already have the root installed.
  const [installTarget, setInstallTarget] = useState<InstallTarget | null>(null);

  useEffect(() => {
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

        const { url, host: displayHost, reachable } = buildRemoteAccessUrl(
          status,
          localIp,
          rootToken,
        );
        setHost(displayHost);
        setUnreachable(!reachable);
        setCertStatus(cert);
        // Build the one-tap install URL from the same realized bind the QR
        // encodes (issue #636). The target device — NOT the desktop host —
        // opens this URL, so the scheme MUST match the bind the phone reaches:
        // a `tauri://` / `https://tauri.localhost` URL pointed at the phone
        // would fail with `ERR_INVALID_URL`. We strip the path off the QR
        // URL and append the install route; reachable matches the same
        // condition as the QR — when the LAN isn't actually exposed, the
        // install button is hidden via `installUrl` being null.
        if (reachable) {
          // Use the URL parser instead of `url.indexOf('/', …)` math —
          // handles IPv6 brackets (`[::1]:1992`) and any future URL shape
          // that adds a path before the query string uniformly.
          setInstallUrl(`${new URL(url).origin}/install-cert.der`);
        } else {
          setInstallUrl(null);
        }

        const dataUrl = await QRCode.toDataURL(url, {
          width: 384,
          margin: 2,
          color: { dark: '#e0e0e0', light: '#1a1a1a' },
        });
        setQrDataUrl(dataUrl);
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

  const handleInstall = (target: InstallTarget) => {
    // One-tap install (issue #636). Same URL for every target — Android
    // Chrome auto-routes the `.der` MIME into the system cert installer,
    // Windows opens the Certificate Import Wizard, macOS Safari/Firefox
    // download the file for a manual Keychain drag. Setting `window.location`
    // (rather than `<a download>` + click) is the only path that works on
    // every platform: `download` is a download attribute (no navigation),
    // and Safari ignores it for cross-origin MIME types.
    if (!installUrl) return;
    // Remember which target so the picker stays expanded while the OS-native
    // installer opens in the background.
    setInstallTarget(target);
    window.location.href = installUrl;
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
            {installUrl && certStatus && (
              <div
                data-testid="remote-access-install"
                className="mt-4 w-full text-left"
              >
                <div className="text-xs text-text-muted mb-2">
                  Install on this device ↓
                </div>
                <div className="grid grid-cols-3 gap-2">
                  {(
                    [
                      { id: 'android', label: 'Android' },
                      { id: 'windows', label: 'Windows' },
                      { id: 'macos', label: 'macOS' },
                    ] as { id: InstallTarget; label: string }[]
                  ).map(opt => (
                    <button
                      key={opt.id}
                      data-testid={`remote-access-install-${opt.id}`}
                      onClick={() => handleInstall(opt.id)}
                      className={`px-3 py-2 rounded text-xs ${
                        installTarget === opt.id
                          ? 'bg-accent-cyan text-bg-base'
                          : 'bg-border-subtle hover:bg-border-default text-text-secondary'
                      }`}
                      type="button"
                    >
                      {opt.label}
                    </button>
                  ))}
                </div>
              </div>
            )}
            {certStatus && (
              <div
                data-testid="remote-access-cert-status"
                className="mt-4 w-full text-left"
              >
                <div className="text-xs text-text-muted mb-1">
                  Server root CA fingerprint
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
