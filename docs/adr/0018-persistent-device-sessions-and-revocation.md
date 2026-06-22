# 18. Persistent Device Sessions & Mobile Revocation

Status: proposed

The Admin surface gains **per-device session tokens**: a paired phone is issued
its own token at pairing, stored hashed in SQLite with last-seen metadata, so
authentication identifies *which device* you are rather than *whether you hold
the one shared secret*. This enables roaming across networks and per-device
revocation. Part of the [Coordinator & Remote Execution Security Hardening PRD
(#494)](https://github.com/alondero/buildmesh/issues/494); issue #502; builds on
#500 / ADR-0015.

## Context

After ADR-0015 the Admin surface authenticated a single **root token**: the
mobile client read it from the desktop QR, POSTed it to `POST /api/session`,
and the server set the `bm_session` cookie to that same root token value. Two
consequences fell out of "one shared secret":

- **No identity.** Every phone presented the same token, so the server could not
  tell devices apart, list them, or revoke one without rotating the root token
  and re-pairing *every* device.
- **IP was incidentally load-bearing.** Nothing tied a session to a device, so
  the only per-connection signal was the peer IP — which changes constantly on
  mobile networks (cellular ↔ Wi-Fi), making any IP-based affinity hostile to
  roaming.

ADR-0015 also reserved the `/admin/*` namespace with no operations mounted,
explicitly so the two-tier separation was enforced and tested *before* the first
admin operation landed. This is that operation.

## Decision

**1. A device session is a first-class, hashed credential.** A new
`device_sessions` table holds one row per paired device: the SHA-256 hash of its
token (never the raw value, mirroring the coordinator tokens — #495), a
`User-Agent`-derived label, the last-seen IP, and created/last-active
timestamps. The raw token is returned to the client exactly once, at pairing.

**2. Pairing mints; re-presentation refreshes.** `POST /api/session` now
branches on the presented bearer token:
- the **root token** (still the QR pairing secret) → *pair*: mint a new device
  session and return its token in the JSON body;
- an existing **device token** → *refresh*: bump `last_active`/`last_ip` and hand
  the same token back.
Either way the `bm_session` cookie is set to the effective device token. The
mobile client persists the returned device token in place of whatever it
presented, so a re-launching phone keeps one identity instead of minting a new
device row per load.

**3. Device tokens resolve to Admin; the IP is demoted to a displayed
attribute.** `resolve_role` accepts a valid device token (cookie or bearer) as
`Role::Admin`, alongside the still-valid root token. Auth no longer consults the
IP at all — it's recorded only for the device list — so a roaming phone stays
authenticated.

**4. Revocation is immediate, for HTTP *and* live WebSockets.** Deleting a
device row blocks its next request (its token stops resolving). For an
already-open WebSocket — which never re-checks auth — the revoke path also fires
the device id onto an in-process `broadcast` channel (`http::revocation`); every
live socket subscribes and closes itself on a matching id. The WS ticket
(ADR-0015) is extended to carry its minting device id so a socket knows which
device it belongs to.

**5. Two surfaces for management, one backend.** The desktop "Authorized
Devices" panel (Tauri commands `list_device_sessions` / `revoke_device_session`)
and the remote `GET /admin/devices` + `POST /admin/devices/{id}/revoke` routes
(Admin-guarded) both back onto the shared `db` + `revocation` layer, so a revoke
from either path behaves identically.

## Consequences

- The root token remains the pairing secret and a valid standing Admin
  credential (the desktop QR re-reads its raw value); moving it out of cleartext
  SQLite into a keychain stays deferred (#495 / PRD #494). It is unrevocable by
  design — there is no device row to delete — so a WS opened with the root token
  is never force-closed.
- "Keychain/Keystore" maps to `localStorage` for the web SPA served at `/v2/` —
  the only client-side store a browser has. A per-device token there is strictly
  better than the shared root token it replaces: its compromise exposes only
  that device, and the user can revoke it.
- `last_active` is refreshed at login and at WS-ticket mint (opening a terminal),
  not on every API poll, to avoid a DB write per poll. A long idle poll loop can
  therefore show a slightly stale "last active" — acceptable for a recognition
  aid.
- Schema moves to v20 (a new table; `CREATE TABLE IF NOT EXISTS` needs no data
  migration). `request::validate_token` and the public `db::validate_root_token`
  wrapper were removed as their only callers migrated to the device-session path.
- TLS for the LAN/VPN path remains a separate slice (ADR-0015 §Consequences);
  this ADR hardens session identity and revocation, not transport encryption.
