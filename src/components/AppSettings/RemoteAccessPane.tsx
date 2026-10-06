/**
 * `RemoteAccessPane` — network reachability: LAN/VPN exposure, the
 * coordinator read API, and authorized devices (issue #1880).
 *
 * All three surfaces are optimistic with rollback, so a toggle never lies
 * about the persisted value. LAN exposure is the interesting one: the
 * backend rebinds listeners live, so after a successful toggle this pane
 * re-reads the realized network status so the bound addresses and TLS
 * state match reality (issue #586).
 *
 * That refresh deliberately skips `lanEnabled`. The optimistic value is
 * the source of truth between writes; overwriting it from the network
 * response races a second toggle whose refresh can land after the first's
 * optimistic flip and revert it.
 *
 * Realized exposure (issue #586) is why the pane shows more than a
 * checkbox: when the toggle is on but the server is still loopback-only
 * (TLS init failed, no interface, or a per-interface bind failed), the
 * user would otherwise hand a phone a dead URL.
 */
import { useState } from 'react';
import * as api from '../../lib/tauri';
import { formatError } from '../../lib/errorUtils';
import { optimisticToggle } from '../../lib/optimisticToggle';
import { SettingsRow, SettingsSection } from './SettingsRow';
import { ResourceLoadStatus } from './ResourceLoadStatus';
import { useSettingsData, type CoordinatorPayload, type NetworkPayload } from './SettingsDataContext';
import type { DeviceSession, RealizedBind } from '../../lib/tauri';

export function RemoteAccessPane() {
  const {
    resources,
    devices: devicesPayload,
    coordinator: coordinatorPayload,
    network: networkPayload,
    loadCoordinator,
    loadDevices,
    retryResource,
    setError,
  } = useSettingsData();

  const [coordEnabled, setCoordEnabled] = useState(false);
  const [coordHasToken, setCoordHasToken] = useState(false);
  const [coordToken, setCoordToken] = useState<string | null>(null);
  const [coordBusy, setCoordBusy] = useState(false);
  const [coordCopied, setCoordCopied] = useState(false);
  const [devices, setDevices] = useState<DeviceSession[]>([]);
  const [confirmingRevokeId, setConfirmingRevokeId] = useState<number | null>(null);
  const [revokingId, setRevokingId] = useState<number | null>(null);
  const [lanEnabled, setLanEnabled] = useState(false);
  const [lanBusy, setLanBusy] = useState(false);
  // Realized exposure (issue #586). Mirrors `lanEnabled` (DB intent) until a
  // mismatch is detected — `lanEnabled=true` with no interfaces means the
  // toggle is on but the server is still loopback-only.
  const [tlsActive, setTlsActive] = useState(false);
  const [exposedInterfaces, setExposedInterfaces] = useState<RealizedBind[]>([]);

  const coordinatorLoaded = resources.coordinator.status === 'loaded';
  const devicesLoaded = resources.devices.status === 'loaded';
  const networkLoaded = resources.network.status === 'loaded';

  // Adopt each committed load *during render* (React's documented
  // adjust-state-while-rendering pattern) rather than from an effect. The
  // resource hook only publishes a payload for a read that still owned its
  // request token, and the identity guard means only a genuinely new load
  // re-seeds. Doing it in an effect would leave one frame where the pane's
  // status says `loaded` while the values it renders are still the previous
  // ones — the same class of bug that made the harness-defaults cards show
  // blanks after a retry.
  const [adopted, setAdopted] = useState<{
    devices: DeviceSession[] | null;
    coordinator: CoordinatorPayload | null;
    network: NetworkPayload | null;
  }>({ devices: null, coordinator: null, network: null });

  if (
    (devicesPayload && devicesPayload !== adopted.devices) ||
    (coordinatorPayload && coordinatorPayload !== adopted.coordinator) ||
    (networkPayload && networkPayload !== adopted.network)
  ) {
    setAdopted({ devices: devicesPayload, coordinator: coordinatorPayload, network: networkPayload });
    if (devicesPayload) setDevices(devicesPayload);
    if (coordinatorPayload) {
      setCoordEnabled(coordinatorPayload.enabled);
      setCoordHasToken(coordinatorPayload.has_token);
    }
    // Issue #586 — the realized fields mirror the network status, but
    // `lanEnabled` is *also* seeded here because this is the only place the
    // persisted intent arrives. The toggle owns the value between writes
    // (see `refreshNetworkStatus`); nothing else writes it until the next
    // committed load.
    if (networkPayload) {
      setLanEnabled(networkPayload.lan_exposure_enabled);
      setTlsActive(networkPayload.tls_active);
      setExposedInterfaces(networkPayload.exposed_interfaces);
    }
  }

  // Re-read the realized network fields after a toggle completes so the
  // realized state (TLS active, exposed interfaces) reflects the
  // post-rebind listeners. Without this the user flips the switch, the
  // optimistic `lanEnabled` flips, but the realized fields stay stale
  // until the next modal open.
  //
  // Issue #1534 (review round 4) — this is NOT the same as
  // `loadNetwork()`. `loadNetwork()` would also overwrite `lanEnabled`
  // from the network response, racing with the optimistic toggle
  // value if a second toggle lands before the read resolves (toggle
  // A's refresh can land after toggle B's optimistic flip and revert
  // B). The optimistic `lanEnabled` is the source of truth between
  // writes; this helper deliberately skips it.
  const refreshNetworkStatus = async () => {
    try {
      const network = await api.getNetworkStatus();
      setTlsActive(network.tls_active);
      setExposedInterfaces(network.exposed_interfaces);
    } catch (e) {
      // Non-fatal — the toggle still reflects the optimistic value. Just log
      // so a future debugging session can correlate the gap.
      console.error('Failed to refresh network status:', e);
    }
  };

  // Revoke a paired device: optimistically drop it from the list, then call the
  // backend (which deletes the row and force-closes any live socket it holds).
  // Roll back on failure so the list never lies about what's still authorized.
  // After a successful revoke, re-fetch the list so the panel stays authoritative
  // — a device may have re-paired, or `last_active_at` may have ticked up, while
  // the user was staring at the modal (issue #595). The refresh is best-effort:
  // a list-fetch failure after a successful revoke must NOT roll back the row,
  // because the revoke did happen — re-showing it would be a worse lie than
  // briefly stale metadata. Mirrors `refreshNetworkStatus`'s pattern (#586).
  const handleRevokeDevice = async (id: number) => {
    const previous = devices;
    setConfirmingRevokeId(null);
    setRevokingId(id);
    setError(null);
    setDevices(prev => prev.filter(d => d.id !== id));
    try {
      await api.revokeDeviceSession(id);
      // Issue #1534 (review round 4) — route the post-revoke
      // refresh through the named loader. A `listDeviceSessions`
      // failure now flips `devicesState` to `failed` and surfaces
      // the banner instead of silently leaving the post-revoke
      // list stale (the revoke DID happen — the loader just
      // rehydrates the metadata). The mutation itself still uses
      // `setError` on the optimistic-rollback path because that's
      // a transient write-failure surface, not a load failure.
      await loadDevices();
    } catch (e) {
      setDevices(previous);
      setError(formatError(e));
    } finally {
      setRevokingId(null);
    }
  };

  // Flip the master kill-switch. Optimistic, with rollback on failure so the
  // toggle never lies about the backend's real state. The shared
  // `optimisticToggle` helper (issue #587) factors out the previous/set/try/
  // catch/finally shape that's repeated by every settings toggle.
  const handleToggleCoordinator = (enabled: boolean) =>
    optimisticToggle({
      current: coordEnabled,
      next: enabled,
      setValue: setCoordEnabled,
      setBusy: setCoordBusy,
      setError,
      // The async wrapper turns `_invoke`'s `Promise<unknown>` into
      // `Promise<void>` so it satisfies the helper's contract.
      mutation: async () => {
        await api.setCoordinatorApiEnabled(enabled);
      },
      // Issue #1534 (review round 5) — refresh via the loader so a
      // backend normalisation (e.g. `has_token` flips when the API
      // gets disabled) surfaces in the coordinator banner. The
      // refresh is best-effort: a failure here must NOT roll back
      // the successful mutation, so we swallow the error inside
      // the wrapper.
      onSuccess: () => {
        loadCoordinator().catch((err) => {
          console.warn('[AppSettings] Failed to refresh coordinator status after toggle:', err);
        });
      },
    });

  // Flip LAN/VPN exposure. The backend rebinds the listeners live (loopback
  // plain HTTP ⇄ LAN interfaces over self-signed TLS), so we await the call
  // and roll back the toggle if it fails. On success we re-read the network
  // status so the realized-state UI (`tls_active`, `exposed_interfaces`)
  // reflects the new bind (issue #586). Same `optimisticToggle` helper as
  // the coordinator toggle — the only delta is the post-success
  // `refreshNetworkStatus` hook (issue #587).
  const handleToggleLanExposure = (enabled: boolean) =>
    optimisticToggle({
      current: lanEnabled,
      next: enabled,
      setValue: setLanEnabled,
      setBusy: setLanBusy,
      setError,
      mutation: async () => {
        await api.setLanExposureEnabled(enabled);
      },
      onSuccess: refreshNetworkStatus,
    });

  // Mint (or replace) the read token. The value is returned exactly once, here —
  // get_coordinator_status only ever reports whether one exists, never its value.
  const handleGenerateToken = async () => {
    setCoordBusy(true);
    setCoordCopied(false);
    setError(null);
    try {
      const token = await api.generateCoordinatorReadToken();
      setCoordToken(token);
      setCoordHasToken(true);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setCoordBusy(false);
    }
  };

  const handleCopyToken = async () => {
    if (!coordToken) return;
    try {
      await navigator.clipboard.writeText(coordToken);
      setCoordCopied(true);
      // Let the "Copied!" confirmation fade back so a second copy reads clearly.
      setTimeout(() => setCoordCopied(false), 2000);
    } catch (e) {
      setError(formatError(e));
    }
  };

  return (
    <>
      <SettingsSection
        title="LAN / VPN Exposure"
        description={
          <>
            Off by default — the server is reachable only from this machine
            (loopback). Enable to let a phone on your LAN or VPN connect. Exposed
            interfaces are served over HTTPS/WSS with a{' '}
            <span className="font-medium">self-signed certificate</span>, so your
            browser will warn the first time you connect; loopback stays plain
            HTTP. The change applies immediately — any currently-connected LAN
            device must reconnect over HTTPS.
          </>
        }
      >
        {/* Issue #1534 — surface network-status failures at the top of
            the LAN section. Without this, the toggle below would be
            disabled with `lanEnabled=false`, and the user might read
            that as "LAN exposure is currently off" — when actually we
            don't know what the persisted value is. The error banner
            makes the unknown state visible. */}
        {!networkLoaded && (
          <div className="mb-4">
            <ResourceLoadStatus
              resource="network"
              state={resources.network}
              onRetry={() => retryResource('network')}
            />
          </div>
        )}

        <SettingsRow
          label="Enable exposure"
          summary="Self-signed TLS; loopback stays plain HTTP."
          controlClassName="w-80 shrink-0"
        >
          <label className="flex items-center gap-3 text-base text-text-primary cursor-pointer">
            <input
              type="checkbox"
              checked={lanEnabled}
              disabled={!networkLoaded || lanBusy}
              onChange={e => handleToggleLanExposure(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
            />
            <span>Expose to LAN / VPN over self-signed TLS</span>
          </label>
        </SettingsRow>

        {/* Realized exposure (issue #586). When the toggle is on but the
            server is still loopback-only (TLS init failed, no interface,
            or per-interface bind failed), warn so the user doesn't hand
            their phone a dead URL. When exposure is working, list the
            actually-bound addresses so the user knows what to type. */}
        {lanEnabled && networkLoaded && (
          <div className="mt-4" data-testid="lan-realized-status">
            {exposedInterfaces.length === 0 ? (
              <div
                className="flex items-start gap-2 bg-bg-card border border-status-warning/40 rounded-md px-3 py-2"
                data-testid="lan-exposure-warning"
                role="alert"
              >
                <span className="text-base text-status-warning">
                  <span className="font-medium">No interfaces are actually exposed.</span>{' '}
                  The toggle is on, but the server didn’t bind any LAN address —
                  either this machine has no non-loopback interface, or the
                  self-signed certificate couldn’t be initialized. Check the
                  application log for details. The loopback listener is still
                  serving plain HTTP for this machine.
                </span>
              </div>
            ) : (
              <div className="border border-border-subtle rounded-lg p-4">
                <div className="text-base text-text-secondary mb-2">
                  Reach the hub from a phone on the same network at:
                </div>
                <ul className="font-mono text-base text-text-primary space-y-1">
                  {exposedInterfaces.map((bind) => (
                    <li key={bind.address} data-testid="lan-exposed-interface">
                      <span className="font-medium">{bind.address}</span>
                      <span className="ml-2 text-text-muted">
                        ({bind.tls ? 'HTTPS/WSS' : 'plain'})
                      </span>
                    </li>
                  ))}
                </ul>
                {!tlsActive && (
                  <div
                    className="mt-3 text-base text-status-warning"
                    data-testid="lan-tls-warning"
                    role="alert"
                  >
                    TLS is not active on any exposed interface — connections
                    from your phone will not be encrypted. Check the
                    application log for the certificate initialization error.
                  </div>
                )}
              </div>
            )}
          </div>
        )}
      </SettingsSection>

      <SettingsSection
        title="Coordinator Read API"
        description={
          <>
            A read-only HTTP view of every node&apos;s status for an external
            coordinator. Off by default. It binds to loopback and your LAN only —
            reaching it from anywhere else is your own tunnel (Tailscale,
            Cloudflare, WireGuard).
          </>
        }
      >
        {/* Issue #1534 — mirror the LAN section: an unknown coordinator
            status must not look like "off" to the user. The toggle is
            already disabled (sees `!coordinatorLoaded`) but the banner
            makes the load failure visible at the section's top. */}
        {!coordinatorLoaded && (
          <div className="mb-4">
            <ResourceLoadStatus
              resource="coordinator"
              state={resources.coordinator}
              onRetry={() => retryResource('coordinator')}
            />
          </div>
        )}

        <SettingsRow
          label="Enable coordinator read API"
          htmlFor="coordinator-read-api"
          summary="Read-only HTTP view of node status for an external coordinator."
        >
          <input
            id="coordinator-read-api"
            type="checkbox"
            checked={coordEnabled}
            disabled={!coordinatorLoaded || coordBusy}
            onChange={e => handleToggleCoordinator(e.target.checked)}
            className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
          />
        </SettingsRow>

        {coordEnabled && (
          <div className="mt-4 border border-border-subtle rounded-lg p-5">
            <div className="flex items-start gap-2 bg-bg-card border border-border-strong rounded-md px-3 py-2 mb-4">
              <span className="text-base text-text-secondary">
                Anyone who can reach this machine on the LAN <span className="font-medium">and</span>{' '}
                holds the token can read your node statuses. The token is shown once, when
                minted — copy it now; regenerating invalidates the old one.
              </span>
            </div>

            <div className="flex gap-3">
              <input
                type="text"
                readOnly
                value={coordToken ?? (coordHasToken ? '••••••••  (a token has already been minted)' : '')}
                placeholder="No token yet — generate one to copy"
                className="flex-1 bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base font-mono text-text-primary focus:outline-none focus:border-accent-cyan"
              />
              {coordToken && (
                <button
                  onClick={handleCopyToken}
                  className="px-5 py-2.5 border border-border-strong text-text-secondary text-base rounded-md hover:bg-bg-card-hover hover:text-text-primary"
                >
                  {coordCopied ? 'Copied!' : 'Copy'}
                </button>
              )}
              <button
                onClick={handleGenerateToken}
                disabled={coordBusy}
                className="px-5 py-2.5 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50 whitespace-nowrap"
              >
                {coordBusy ? 'Working…' : coordHasToken ? 'Regenerate token' : 'Generate token'}
              </button>
            </div>
          </div>
        )}
      </SettingsSection>

      <SettingsSection
        title="Authorized Devices"
        description={
          <>
            Phones you&apos;ve paired keep their own session token, so they stay
            connected as their network (and IP) changes. Revoke any device to cut
            it off immediately — its open connections drop and it must pair again
            with a fresh QR code.
          </>
        }
      >
        {!devicesLoaded ? (
          <ResourceLoadStatus
            resource="devices"
            state={resources.devices}
            onRetry={() => retryResource('devices')}
          />
        ) : devices.length === 0 ? (
          <p className="text-base text-text-muted italic">
            No paired devices yet. Scan the Remote Access QR code from a phone to pair one.
          </p>
        ) : (
          <ul className="flex flex-col gap-2">
            {devices.map(device => (
              <li
                key={device.id}
                className="flex items-center gap-4 border border-border-subtle rounded-lg px-4 py-3"
              >
                <div className="min-w-0 flex-1">
                  <div className="text-base text-text-primary truncate">
                    {device.label ?? 'Unknown device'}
                  </div>
                  <div className="text-sm text-text-muted truncate">
                    {device.last_ip ?? 'IP unknown'} · last active {device.last_active_at}
                  </div>
                </div>
                {confirmingRevokeId === device.id ? (
                  <div className="flex gap-2 whitespace-nowrap">
                    <button
                      onClick={() => handleRevokeDevice(device.id)}
                      disabled={revokingId === device.id}
                      className="px-4 py-2 bg-status-error text-white text-base rounded-md hover:bg-status-error/90 disabled:opacity-50"
                    >
                      {revokingId === device.id ? 'Revoking…' : 'Confirm revoke'}
                    </button>
                    <button
                      onClick={() => setConfirmingRevokeId(null)}
                      className="px-4 py-2 bg-bg-card text-text-secondary text-base rounded-md hover:bg-border-subtle"
                    >
                      Cancel
                    </button>
                  </div>
                ) : (
                  <button
                    onClick={() => setConfirmingRevokeId(device.id)}
                    className="px-4 py-2 bg-status-error/15 text-status-error text-base rounded-md hover:bg-status-error/25 whitespace-nowrap"
                  >
                    Revoke
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
      </SettingsSection>
    </>
  );
}