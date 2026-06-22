# 15. Two-Tier API Roles (RBAC) & Header Auth

Status: proposed

The embedded HTTP/WS server gains an explicit two-role authorization model and
moves every credential off the URL into headers/cookies, with a ticket exchange
for the WebSocket handshake. Part of the [Coordinator & Remote Execution
Security Hardening PRD (#494)](https://github.com/alondero/buildmesh/issues/494);
issue #500; unblocked by #495 (token hashing).

## Context

The server authenticated with two parallel, unaware token systems: the **root
token** (the mobile/admin surface — `/api/*`, the SPA shell, both WebSockets)
and the **coordinator tokens** (read + drive — `/nodes*`, ADR-0008). Both
accepted credentials via a `?token=` URL parameter. URL tokens leak into server
logs, browser history, and `Referer` headers, and there was no unified notion of
*role*: nothing stopped a future admin endpoint from being reachable by a
coordinator token, and the two surfaces' separation was incidental (different
token values) rather than enforced.

## Decision

**1. Two roles, disjoint surfaces.** A request resolves to exactly one
[Role](../../CONTEXT.md): **Admin** (the root token) or **Coordinator**
(read- or drive-scoped tokens). The roles are *not* a privilege hierarchy — they
are separate surfaces. The root token owns `/api/*` + the WebSockets and is
**not** accepted on the coordinator routes; coordinator tokens own `/nodes*` and
are **not** accepted on the admin surface. Within the coordinator surface, drive
implies read.

**2. 401 vs 403 are distinct.** No valid credential → `401 Unauthorized`. A valid
credential of the *wrong* role → `403 Forbidden`. This is what makes "coordinator
tokens are strictly blocked from `/admin`" observable: a coordinator token to an
admin route is a deliberate 403, not an ambiguous 401.

**3. Reserved `/admin` namespace.** `/admin/*` is reserved and guarded
Admin-only now, even though no remote admin operations are mounted yet (token
rotation, kill-switches, settings remain desktop-only Tauri commands). The guard
exists so the two-tier separation is enforced and tested before any such
operation is exposed: coordinator → 403, no creds → 401, Admin → 404.

**4. Header/cookie auth only — `?token=` is removed.** Credentials arrive as
`Authorization: Bearer <token>` (the coordinator surface and the login handoff)
or the HttpOnly `bm_session` cookie (the mobile surface after login). The
`?token=` URL parameter no longer validates anywhere.

**5. Public shell + login handoff.** The SPA shell (`GET /`) and `/assets/*` are
served **publicly** — they hold no secrets and must load so their JS can run.
That JS reads the token from the QR/paste and POSTs it to **`POST /api/session`**
(`Authorization: Bearer`), which validates it and sets the `bm_session` cookie,
then strips the token from the URL. This replaces the old `/?token=` →
Set-Cookie bootstrap; the token never reaches a query parameter the server
validates. The DNS-rebinding `Host` guard (ADR-0012/#496) still runs first on
every request, and all data APIs remain authenticated.

**6. WebSocket ticket exchange.** A browser cannot set headers on a WS upgrade,
and proxies strip cookies on it — the original reason the long-lived token rode
the URL. Instead, an authenticated **`POST /api/ws-ticket`** mints a short-lived
(30s), single-use ticket held in memory; the client passes it as `?ticket=` on
the upgrade. Because a ticket can only be obtained through a cookie/header-
protected fetch, a cross-site page cannot mint one — closing the cross-site
WebSocket hijacking hole a raw cookie-on-WS would leave open. A raw `?token=` on
the upgrade is no longer honoured.

The ticket is **bound at mint time to the target** the caller will open (issue
#551): the `POST /api/ws-ticket` body names a surface (`terminal` or `events`)
and, for `terminal`, the node `id`. The upgrade reconstructs the requested target
from its URL and rejects a ticket whose binding doesn't match with `403
Forbidden` — and crucially does **not** consume it, so a misrouted legitimate
client can retry against its real target within the 30s window. The **binding**,
not the TTL, is the trust boundary here: a 30-second single-use window still lets
a leaked-but-unbound ticket open any node the minting role could read; binding
narrows the blast radius to the one target the caller asked for, and makes a leak
observable (the attacker's parallel upgrade consuming the ticket surfaces as an
error on the legitimate client's own attempt). A browser WebSocket cannot read
the upgrade's HTTP status, so the `401`/`403` distinction is for the server and
its logs; the client's *actionable* failures come from the cookie-authenticated
mint fetch (`400` for a malformed target, `401`/`403` for a stale cookie → bounce
to Connect).

## Consequences

- The mobile client (`src/mobile/`) logs in via `POST /api/session` and mints a
  ticket per WebSocket connection; its `apiFetch` relies purely on the cookie.
- Role resolution lives in `src-tauri/src/http/auth.rs` (`Role`,
  `RequiredScope`, `AuthOutcome`, `authorize`, `guard`); the dispatcher
  (`http/mod.rs`) calls `auth::guard(.., scope)` at each route. The ticket store
  is `src-tauri/src/http/ws_ticket.rs`. The coordinator's bearer parser was
  promoted to `request::bearer_token`; its standalone `authenticate_read`/
  `authenticate_drive` helpers were absorbed into the unified guard.
- The root token remains stored cleartext (the QR re-reads its raw value); moving
  it to a hashed/Keychain store stays deferred (#495 / PRD #494).
- TLS for the LAN/VPN path is still a separate slice; this ADR hardens auth shape
  and role separation, not transport encryption.
