import { useState, useEffect } from 'react';
import QRCode from 'qrcode';
import * as api from '../../lib/tauri';
import type { NetworkStatus } from '../../types/generated/NetworkStatus';

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
 */
export function buildRemoteAccessUrl(
  status: NetworkStatus,
  fallbackIp: string,
  rootToken: string,
): { url: string; host: string; reachable: boolean } {
  // Prefer a TLS listener (what the phone should hit over HTTPS); fall back to
  // any realized bind so an unexpected plain LAN listener still gets the right
  // scheme rather than a mismatched one.
  const bind =
    status.exposed_interfaces.find(b => b.tls) ?? status.exposed_interfaces[0];
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

  useEffect(() => {
    const init = async () => {
      try {
        // These three reads are independent — dispatch them together rather than
        // serially so the modal opens in one round-trip. The `getLocalIp`
        // fallback string is a UX placeholder, not a typed contract: it is only
        // used when no LAN listener is bound, in which case `reachable` is false
        // and we warn rather than rely on it.
        const [rootToken, status, localIp] = await Promise.all([
          api.getRootToken(),
          api.getNetworkStatus(),
          api.getLocalIp().catch(() => '192.168.1.x'),
        ]);

        const { url, host: displayHost, reachable } = buildRemoteAccessUrl(
          status,
          localIp,
          rootToken,
        );
        setHost(displayHost);
        setUnreachable(!reachable);

        const dataUrl = await QRCode.toDataURL(url, {
          width: 240,
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

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" onClick={onClose}>
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/70" />

      {/* Modal */}
      <div
        className="relative bg-bg-overlay border border-border-default rounded-lg shadow-2xl p-6 max-w-sm w-full"
        onClick={e => e.stopPropagation()}
      >
        <button
          onClick={onClose}
          className="absolute top-3 right-3 text-text-muted hover:text-text-secondary text-lg"
        >
          ×
        </button>

        <h2 className="text-sm font-semibold text-text-primary mb-1">Remote Access</h2>
        <p className="text-xs text-text-muted mb-4">
          Scan with your phone camera to connect.
        </p>

        {error ? (
          <div className="text-status-error text-xs">{error}</div>
        ) : unreachable ? (
          <div
            data-testid="remote-access-warning"
            role="alert"
            className="text-status-error text-xs"
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
              className="w-48 h-48 rounded border border-border-subtle mb-3"
            />
            <div className="text-xs text-text-muted font-mono text-center">
              <div>{host}</div>
            </div>
          </div>
        ) : (
          <div className="flex justify-center py-8">
            <div className="animate-spin w-5 h-5 border border-accent-cyan border-t-transparent rounded-full" />
          </div>
        )}

        <div className="mt-4 pt-3 border-t border-border-subtle">
          <p className="text-[10px] text-text-muted">
            Make sure your phone is on the same network as this computer.
          </p>
        </div>
      </div>
    </div>
  );
}
