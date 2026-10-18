import { _invoke } from './_invoke';
import type { CoordinatorStatus } from '../../types/generated/CoordinatorStatus';
import type { DeviceSession } from '../../types/generated/DeviceSession';
import type { NetworkStatus } from '../../types/generated/NetworkStatus';
import type { RealizedBind } from '../../types/generated/RealizedBind';
import type { CertChainStatus } from '../../types/generated/CertChainStatus';

// ── Coordinator read API control (ADR-0008) ────────────────────────────────
//
// Generated from `commands::coordinator::CoordinatorStatus` (issue #404).
// `has_token` reports presence without ever leaking the token value — the
// token is only ever surfaced once, by `generateCoordinatorReadToken`.
export type { CoordinatorStatus };

export const getCoordinatorStatus = () =>
  _invoke<CoordinatorStatus>('get_coordinator_status');

export const setCoordinatorApiEnabled = (enabled: boolean) =>
  _invoke('set_coordinator_api_enabled', { enabled });

/** Mint (or replace) the read-scoped token and return it for the user to copy.
 *  Replacing invalidates the previously issued token; the value is shown once. */
export const generateCoordinatorReadToken = () =>
  _invoke<string>('generate_coordinator_read_token');

// ── Authorized devices (issue #502) ────────────────────────────────────────
//
// Per-device session tokens minted at mobile pairing. The list omits the token
// hash (the secret never crosses IPC); revoking deletes the device and kicks any
// live socket it holds, so revocation takes effect immediately.
export type { DeviceSession };

export const listDeviceSessions = () =>
  _invoke<DeviceSession[]>('list_device_sessions');

export const revokeDeviceSession = (id: number) =>
  _invoke('revoke_device_session', { id });

// ── LAN/VPN exposure & self-signed TLS (issue #501) ────────────────────────
//
// Off by default: the server binds loopback only. Enabling exposure binds the
// machine's LAN interfaces over self-signed TLS (HTTPS/WSS) and rebinds live —
// no app restart. `getNetworkStatus` reports the switch and the bound port;
// `RealizedBind` (issue #586) is one element of its realized-listener list,
// used by the Settings UI to show *actual* exposure rather than just DB intent.
export type { NetworkStatus, RealizedBind };

export const getNetworkStatus = () =>
  _invoke<NetworkStatus>('get_network_status');

export const setLanExposureEnabled = (enabled: boolean) =>
  _invoke('set_lan_exposure_enabled', { enabled });

// ── Remote access (mobile QR) ──────────────────────────────────────────────
export const getLocalIp = () =>
  _invoke<string>('get_local_ip');

export const getRootToken = () =>
  _invoke<string>('get_root_token');

export const createPairingTicket = () =>
  _invoke<string>('create_pairing_ticket');

export type { CertChainStatus };

export const getCertChainStatus = () =>
  _invoke<CertChainStatus>('get_cert_chain_status');

/** Explicit "Reset trusted certificates" action (issue #1527). Wipes the
 *  persisted root + leaf + SAN sidecar + generation counter so the next
 *  bind mints a fresh root. The user's phone loses trust and must
 *  re-install via the install-QR. Returns the new `root_generation` so
 *  the UI can confirm the rotation took and re-fetch cert_status. The
 *  backend command also invalidates the live `TlsAcceptor` cache and
 *  triggers a rebind, so the iOS `.mobileconfig` QR (which the desktop
 *  re-mints after a reset) actually chains to the cert the listener is
 *  now serving.
 *
 *  **Idempotent**: calling on an already-empty tls/ dir is a no-op
 *  generation bump. Frontend exposes this as a Settings affordance;
 *  never auto-invoked from network-change paths.
 */
export const resetTrustedCertificates = () =>
  _invoke<number>('reset_trusted_certificates');

/** Root CA bytes for the phone-install QR (issue #702). Returns base64
 *  (standard alphabet, '=' padding) — concatenate with the data: prefix
 *  to produce the OS-installable URL. The desktop modal embeds this in
 *  a second QR; scanning the QR on Android/iOS routes through the OS
 *  CA installer instead of opening /install-cert.der in the desktop's
 *  WebView2. */
export const getRootCertDer = () =>
  _invoke<string>('get_root_cert_der');

/** Signed `.mobileconfig` profile for the iOS install-QR (issue #713).
 *  Returns base64 of a DER-encoded PKCS#7/CMS SignedData wrapping the
 *  unsigned Apple Configurator 2 plist — the same wire format as
 *  `openssl cms -sign -binary -outform DER -nodetach`. The frontend
 *  concatenates `data:application/x-apple-aspen-config;base64,` to
 *  produce the data: URL Safari intercepts on iOS ≥ 14. Sibling to
 *  `getRootCertDer` (the Android path) — kept as a separate command
 *  rather than a parameter so the failure surfaces cleanly per platform
 *  and the modal can hide the iOS tab on its own rejection without
 *  affecting the Android one. */
export const getRootCertMobileconfig = () =>
  _invoke<string>('get_root_cert_mobileconfig');
