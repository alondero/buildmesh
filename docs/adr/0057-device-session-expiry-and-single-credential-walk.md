# 57. Device Sessions Expire, and One Credential Walk Answers Everything

Status: accepted

## Context

[ADR 0018](0018-persistent-device-sessions-and-revocation.md) made a paired
phone a first-class credential: its own token, stored hashed in
`device_sessions`, revocable on its own. That record says the row carries
"created/last-active timestamps" — and then never used them. Nothing aged a
device session out.

The consequence is a class of orphan. A phone that re-pairs — cleared site
data, a new browser, a reinstall, a second QR scan — gets a *new* row. The
old token is never presented again, so the old row stays in the "Authorized
Devices" panel as a stale duplicate that still authenticates as **Admin**. The
panel fills with rows for the same physical phone, and every one of them is a
live credential until the user notices and revokes it by hand. Nothing in the
product ever aged them out.

Two smaller problems sat alongside it:

- **A stale `last_active_at`.** The record says the timestamp is refreshed at
  login and at WebSocket-ticket mint, "not on every API poll, to avoid a DB
  write per poll". That is the right call for a write, but it left the
  timestamp unable to distinguish *idle* from *in use*: a phone that polls for
  months without a reboot looks identical to a phone that was abandoned years
  ago. A TTL built on that timestamp would have swept live devices.
- **Two credential walks.** `POST /api/ws-ticket` resolved the request's role
  and then walked the credentials again for the device id the ticket had to be
  bound to. Two DB checkouts per mint, and a window in which the two passes
  could disagree — a revoke landing between them would bind the ticket to a
  device the request was not authorized as.

## Decision

1. **A device session expires after an idle window.**
   `services::device_session_expiry::DEVICE_SESSION_IDLE_DAYS` (90 days) is
   the single source of truth. A session that has not authenticated for that
   long is deleted: its token stops resolving, and its row leaves the panel.
   The window is deliberately shorter than the 400-day session cookie, so a
   phone that comes back after a long absence is asked to re-pair rather than
   being silently trusted again.
2. **The sweep runs at startup and every six hours.** It is a single bounded
   `DELETE` over a single-digit-row table, so running it on every launch costs
   nothing — and an install that was closed for months must not come back to a
   panel full of orphans.
3. **Expiry is measured by activity, and activity is refreshed by use.** An
   authenticated request refreshes the row's `last_active_at` when it is more
   than an hour stale, so a device that keeps polling keeps its row no matter
   how old its pairing is. The window is an hour rather than the idle window
   so a busy client is not writing on every poll, and short enough that the
   panel's "last active" reading stays honest.
4. **An expired session is revoked, not merely forgotten.** The sweep fires the
   deleted device id onto the same in-process revocation channel the desktop
   revoke path uses, so an already-open WebSocket closes too. A deleted row
   alone would only stop the *next* request.
5. **One credential walk answers both questions.** The HTTP auth layer
   resolves the role *and* the device id together, in one walk against one
   connection. The WebSocket-ticket mint binds the ticket to the device that
   same walk reported. The two can no longer disagree, and a mint costs one
   checkout instead of two. A SQLite failure during the walk is an
   *unavailability* (`503`), never "no valid credential" (`401`) — a 401
   signs the phone out and clears its stored credential, so a storage fault
   must never read as a revoked device.
6. **The panel shows the pairing date and a relative last-active age.** An
   old pairing date next to a long idle age is the shape of an orphan, so the
   user can spot and clean one without reading raw timestamps.

## Alternatives considered

- **De-duplicate on pairing instead of expiring.** Rejected: the only
  fingerprint available at pairing time is a `User-Agent`-derived label, and
  two different phones routinely report the same one. De-duplicating on it
  would let a second phone evict a legitimate device's session — trading a
  stale credential for a live takeover. A stable per-install identifier
  supplied by the client would be the honest fingerprint, and it is a larger
  change to the mobile client than this record covers.
- **A short TTL (7–30 days).** Rejected: it would sign out phones that are
  merely used infrequently, and it is shorter than the patterns that produce
  orphans in the first place. Ninety days is long enough that a device in
  regular use is never at risk.
- **Expire by pairing age rather than by activity.** Rejected: a device paired
  years ago and used yesterday would be swept. Age of the *pairing* says
  nothing about whether the credential is still in use.
- **Refresh `last_active_at` on every request.** Rejected: a DB write per
  poll, on a phone client that polls continuously. The throttled refresh keeps
  the signal without the write storm.
- **Let the ledger GC worker own the sweep.** Rejected: the ledger GC is
  bounded-age garbage collection of append-only history rows; deleting a
  device session is a credential revocation with live-socket consequences.
  Different kind of sweep, different owner.
- **Leave the two credential walks in place.** Rejected: the duplicate
  checkout is pure cost, and the disagreement window is a correctness bug
  (a ticket bound to a device the request was not authorized as).

## Consequences

- A device session is no longer permanent. A phone that is genuinely gone
  stops being an Admin credential after the idle window, without the user
  having to notice and revoke it.
- A phone that is away for longer than the idle window must re-pair with a
  fresh QR code. Its cookie is still syntactically valid; the server no
  longer recognises the token behind it.
- The "Authorized Devices" panel now shows a relative last-active age and
  the pairing date, so a stale duplicate is recognisable at a glance.
- `device_sessions` keeps its schema. The sweep is a `DELETE` over an
  existing column, so no migration and no version bump are involved.
- The root token is unaffected: it owns no device row, is unrevocable by
  design, and is never swept.

## Not covered

De-duplicating rows at pairing time — replacing a stale row for the same
physical device rather than letting it age out — needs a client-supplied
per-install identifier, which is a change to the mobile client and to the
pairing contract rather than to the server's expiry policy. This record
covers the server side only.

## Links

- [Remote access and the Coordinator API](../development/remote-access.md) —
  the developer-facing description of the device-session lifecycle.
- [ADR 0018](0018-persistent-device-sessions-and-revocation.md) — the record
  that introduced per-device session tokens.
- [ADR 0034](0034-pairing-tickets-and-trusted-root-rotation.md) — the current
  pairing-ticket and trusted-root contract.
