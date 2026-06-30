# 17. Opt-In LAN Exposure & Self-Signed TLS

Status: accepted

The embedded HTTP/WS server gains an explicit, off-by-default switch that exposes
it beyond loopback, and when exposed it serves the LAN-facing interfaces over
HTTPS/WSS with a self-signed certificate. Part of the [Coordinator & Remote
Execution Security Hardening PRD (#494)](https://github.com/alondero/buildmesh/issues/494);
issue #501; unblocked by #496 (loopback default + Host validation). Unblocks the
`Secure`-cookie slice (#553).

## Context

Since #496/ADR-0012 the server binds loopback only by default; reaching it from a
phone on the LAN was a DB flag (`lan_exposure_enabled`) with no UI, no transport
encryption, and no live effect — it was only read once at startup. Exposing the
hub on the LAN as plain HTTP means the root token, terminal traffic, and node
data cross the wire in the clear, readable by anyone on the same network. To make
LAN/VPN access safe we need (a) a real user-facing opt-in, (b) transport
encryption on the exposed path, and (c) the toggle to take effect without an app
restart.

## Decision

**1. Opt-in, off by default, stored in the DB.** `lan_exposure_enabled`
(`app_settings`) defaults to `false`. A "LAN / VPN Exposure" toggle in App
Settings drives the `set_lan_exposure_enabled` Tauri command; `get_network_status`
reports the switch and the bound port.

**2. Loopback stays plain HTTP; only the interfaces get TLS.** When exposure is
on we do **not** bind `0.0.0.0` with TLS. Instead the primary loopback listener
(`127.0.0.1`, plus best-effort `::1`) stays **plain HTTP**, and each
**non-loopback interface IP** is bound as a **TLS** listener. This is the same
reachability as `0.0.0.0` for a LAN client, but it keeps the local **attention
webhook** working: Claude Code's Stop/Notification hooks `POST` to
`http://localhost:$BUILDMESH_PORT/api/attention/...` (plain, peer-loopback-gated),
and forcing TLS on that loopback path would silently break every agent's
"awaiting input" signal. The loopback listener also remains load-bearing for the
1992→1994 port resolution. See `http::bind_specs`.

**3. Self-signed certificate, persisted.** A self-signed cert is generated with
`rcgen` (ring backend) carrying SANs for `localhost`, both loopback IPs, and every
non-loopback interface IP, and persisted as DER under `<app-data>/tls/`. Persisting
means a phone that trusted the cert once keeps trusting it across restarts;
deleting the `tls/` directory is the supported "rotate the cert" gesture. The
browser will still warn on first connect (self-signed is not a trusted CA) — that
is expected and surfaced in the UI copy. See `http::tls`.

**4. The toggle rebinds live.** Flipping the switch tears down the current
listeners (signalling a `watch` shutdown channel and awaiting the accept-loop
tasks so the sockets are dropped and the port frees) and binds fresh ones for the
new setting — no app restart. Existing loopback connections are unaffected;
existing LAN connections drop and must reconnect over HTTPS. See
`http::apply_binding` / `reapply_binding`.

**5. One stream type, not generics.** The server parses requests over a raw
stream and every route handler is typed against one concrete stream. Rather than
make all handlers generic over `AsyncRead + AsyncWrite`, a `MaybeTls` enum
(`Plain(TcpStream)` | `Tls(Box<TlsStream<TcpStream>>)`) implements the async IO
traits and is that one type. WSS comes for free: the WebSocket upgrade wraps
whichever `MaybeTls` it was handed. See `http::stream`.

**6. Crypto provider is `ring`, selected explicitly.** `rustls`/`tokio-rustls`/
`ring` already ride in transitively (reqwest's `hyper-rustls`), but with no crypto
provider compiled in and no aws-lc-rs in the tree. We enable the `ring` provider
and build every `ServerConfig`/`ClientConfig` with `builder_with_provider(ring)`
so TLS never depends on a process-default provider.

**7. The interface snapshot refreshes on each bind (issue #585).** The bind path
calls `refresh_local_interface_ips` (which re-runs `local_ip_address::
list_afinet_netifas` and replaces a `parking_lot::RwLock<Vec<IpAddr>>` cache)
before constructing `bind_specs` and the TLS cert. A VPN or Wi-Fi adapter that
connects **after** the first enumeration is therefore picked up on the next
LAN-toggle rebind — no app restart. The per-request Host guard still reads the
cached snapshot via a shared-lock + `Vec::clone`; it never pays for enumeration,
which can stall for seconds behind a VPN/Docker stack on Windows. The old
`0.0.0.0` wildcard bind (replaced in #501) had this for free — every interface
the box held at the moment of `accept` was reachable — so the refresh is the
smallest change that restores that reachability for per-interface TLS binds.

**8. OS-level interface events drive the rebind (issue #591).** Even with the
per-bind refresh in §7, the rebind only fires when something *triggers* it — a
user LAN toggle or startup. An interface that appears **without** a user action
(VPN connecting mid-session, DHCP lease change, Wi-Fi reconnect) was
previously not picked up until the user re-toggled LAN exposure or restarted.
The `http::interface_watcher` module closes this: it listens for OS-level
interface notifications and re-runs `apply_binding` after a 250 ms debounce,
so one network change produces one rebind — not one per kernel signal.

The split is platform-specific:

- **Windows** — `NotifyAddrChange` from `Iphlpapi.dll`, called via
  `tokio::task::spawn_blocking`. The syscall blocks the thread until any
  address change occurs; the watcher loops so a single watcher call outlives
  any number of changes. Returns `NO_ERROR` on a successful wake-up; non-zero
  logs and exits (the next user toggle recovers). `windows-sys 0.61` is pinned
  (the crate is already in the tree transitively via `rustix`/`signal-hook`).
- **macOS / Linux** — polling fallback. The watcher re-enumerates interfaces
  every second via `list_afinet_netifas` and emits only when the snapshot
  differs from the previous one, so a stable network stays silent. The 1 s
  latency is invisible to the user; the debouncer collapses DHCP bursts.

The watcher callback reads `lan_exposure_enabled` from the DB and is a no-op
when LAN exposure is off — a user who has never opted in pays nothing for the
background work. When LAN exposure is on, the callback reuses the existing
`apply_binding` path; TLS cert regeneration, SAN update, and `bind_specs`
rebuild are unchanged. Tests drive the debouncer directly through an
`mpsc::Sender<()>` so the production source code path is exercised by smoke-
test, not unit-test.

## Consequences

- The mobile client connects to `https://<lan-ip>:<port>` (WSS for terminals)
  when exposure is on, accepting the self-signed cert once. The QR/remote-access
  port is unchanged by the toggle.
- The session cookie is still set **without** `Secure`: the loopback listener is
  always plain HTTP, so a blanket `Secure` flag would break it. Gating `Secure`
  on the request scheme is the follow-up #553, now unblocked.
- If TLS init fails, the exposed interfaces are **not** bound (the server never
  falls back to serving plaintext on an interface meant to be TLS); loopback
  still serves.
- LAN exposure with no non-loopback interface (e.g. offline) is a safe no-op:
  loopback-only, no TLS listener.
- The persisted cert is reused only while it still **covers** the current
  interface IPs (a `tls/sans.txt` sidecar records what it was minted for). A LAN
  IP change (DHCP, VPN, new subnet) forces regeneration so a client never hits a
  SAN/IP mismatch; a shrunk set keeps the cert (a stale extra SAN is harmless).
- The DNS-rebinding `Host` guard (ADR-0012/#496) and the two-tier header auth
  (ADR-0015/#500) are unchanged and still run on every request, TLS or not.
- **Interface changes mid-session are picked up automatically (issue #591).**
  The watcher re-runs `apply_binding` on every debounced OS event, so the
  realised bind set stays in lockstep with the host's actual interface set
  without user action. A burst of DHCP renews (the noisy case) collapses to
  a single rebind; the user never sees a stale QR URL.

### Known limitations (tracked follow-ups)

- **Realized exposure is not reported.** `get_network_status` returns the DB
  intent (`lan_exposure_enabled`), not whether interface listeners actually bound.
  If TLS init fails or there is no non-loopback interface, the UI still shows
  "enabled" with no signal that nothing is exposed. Surfacing the realized bind
  state is a follow-up.
