import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import QRCode from 'qrcode';

interface RemoteAccessModalProps {
  onClose: () => void;
}

async function getLocalIp(): Promise<string> {
  try {
    const ip = await invoke<string>('get_local_ip');
    return ip;
  } catch {
    return '192.168.1.x';
  }
}

export function RemoteAccessModal({ onClose }: RemoteAccessModalProps) {
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [ip, setIp] = useState<string>('discovering...');
  const [error, setError] = useState<string | null>(null);
  const PORT = 1992;

  useEffect(() => {
    const init = async () => {
      try {
        const rootToken = await invoke<string>('get_root_token');
        const localIp = await getLocalIp();
        setIp(localIp);

        const url = `http://${localIp}:${PORT}/?token=${rootToken}`;
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
        ) : qrDataUrl ? (
          <div className="flex flex-col items-center">
            <img
              src={qrDataUrl}
              alt="QR Code"
              className="w-48 h-48 rounded border border-border-subtle mb-3"
            />
            <div className="text-xs text-text-muted font-mono text-center">
              <div>{ip}:{PORT}</div>
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
