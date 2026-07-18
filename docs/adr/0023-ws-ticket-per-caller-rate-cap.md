# 23. Per-caller rate cap on `POST /api/ws-ticket`

Status: proposed

`POST /api/ws-ticket`, which mints the single-use WebSocket handshake ticket
that gates every WS upgrade, gains a per-token sliding-window rate cap
(30 mints per 60 s per token by default). Part of the
[Coordinator & Remote Execution Security Hardening PRD (#494)](https://github.com/alondero/buildmesh/issues/494);
issue #552; unblocked by #500 (the ticket endpoint itself).

## Context

`POST /api/ws-ticket` (issues #500, #551) is the only minting path for the
short-lived, single-use tickets that gate `/ws/terminal/{id}` and `/ws/events`
upgrades. Each successful mint allocates an in-memory entry the upgrade later
consumes. The endpoint authenticates via the `bm_session` cookie or the
`Authorization: Bearer` header, so a token holder — the desktop admin or a
mobile device paired to it — can flood the mint and either DoS the route or
churn the in-memory map.

The map has a TTL-based pruning policy but no per-caller ceiling: a token
holder that fires 1 000 mints/sec can keep the table near cap permanently and
crowd out legitimate reconnects. PRD #494 called out "lacks authentication
rate limiting" as a headline gap; #500 closed the auth shape (tokens off the
URL, hashed at rest) but explicitly deferred per-caller throttling as a
follow-up slice.

## Decision

**1. In-memory per-token counter.** A new module
[`http::rate_limit`](../../src-tauri/src/http/rate_limit.rs) records each
mint as an `Instant` keyed by the SHA-256 of the bearer / cookie credential.
A process-local `OnceLock<RwLock<HashMap<String, Vec<Instant>>>>` is exactly
the same shape `super::ws_ticket::TICKETS` uses — adding a second map is
structurally familiar and inherits the no-background-sweep property (the
counter is pruned on every read).

**2. Default 30 mints / 60 s / token.** Sized for a phone's reconnect storm
of one (issue wording: "~30/minute/token, to be tuned"). At 1 mint/sec, the
30th in any rolling minute is denied. A 60 s **sliding window** is preferred
over a fixed minute bucket: a fixed boundary lets a client burn 30 in the
last second of one minute and 30 in the first second of the next, doubling
the effective cap.

**3. Counter is process-local.** The state is intentionally not in SQLite:
a hot-path SQLite write per mint would amplify the very DoS class this
feature exists to bound, and the ticket map (`ws_ticket::TICKETS`) is also
memory-only. A process restart is the documented way to clear the counter;
this matches PRD #494's broader "in-memory defense, persistent storage is for
durability" stance.

**4. 429 BEFORE the auth guard.** The rate-limit check runs ahead of
`auth::guard` in the dispatcher. This is deliberate: a flooder with a stolen
token can't keep burning mints, and the cap protects the DB lookup that
`resolve_role` does for any *presented* credential. The 429 body is
**deliberately identical in shape to the 401 body** (`Content-Length: 0`,
empty body): the only signals a caller can read off a 429 are the status
line and the `Retry-After` header. A 429 from a stolen token is
indistinguishable from a 401 to the attacker.

**5. `Retry-After: N` is a delta-seconds integer (`>= 1`).** Computed from
the oldest entry in the bucket — the soonest the cap can possibly free a
slot. A 1-second floor prevents `Retry-After: 0` (which would invite an
instant retry loop).

**6. Mobile client retries exactly once.** A new helper
[`mintWsTicketWithBackoff`](../../src/mobile/api.ts) waits the
server-suggested hint (capped at 2 s so a fresh-window hint doesn't freeze
the UI), retries once, and on a second 429 throws `ApiError(429, "Server is
busy — reconnecting.")`. NOT infinite — the issue's AC is explicit on this
("does not loop").

**7. UI surfaces 429 as a "server is busy" toast, not a reconnect banner.**
A 429 isn't a connectivity failure; counting it against the existing
1/2/4/8/16 s reconnect ladder would be mis-information. The terminal screen
shows a dedicated "Server is busy — please try again." banner with a manual
Retry button; the events stream simply stops retrying (the 5 s poll that
backs it still surfaces fresh state, so a brief stall is invisible).

## Why per-token, not per-IP

The credential is a high-entropy token, the IP is something a phone's
NAT/VPN reshuffles on every reconnect. Capping per IP would penalise a
legitimate phone that roams between Wi-Fi and cellular; per-token matches
the *thing a leaked credential could abuse* (the same token from the same
peer or another) and gives revocation a free pass (deleting a device row
stops the next auth; the counter ages out the rest within the window).

## Why no logger / metric for 429 yet

A `tracing::warn!` line per denied mint would itself be a flooding vector
under attack. If/when structured metrics for this surface become warranted
(a separate issue), the rate-limit module's `Outcome::Deny` branch is the
one place to add a sampler.

## Consequences

- The auth layer's [`auth::guard`](../../src-tauri/src/http/auth.rs) is
  unchanged; the rate cap sits in front of it. Cookie- and bearer-presented
  requests of the *same credential* share one bucket (their SHA-256
  fingerprints match).
- The dispatcher's `route_table_scope_snapshot` test (in
  [`http::mod::tests`](../../src-tauri/src/http/mod.rs)) needed no change:
  the rate limit is a response-shape modifier, not a new route.
- Mobile callers of `mintWsTicket()` directly (none today) will see the new
  429 contract — `isRateLimited(e)` mirrors `isAuthError(e)` for that case.
- A future broader rate-limit pass over the rest of the HTTP surface (the
  follow-up the issue alludes to) can reuse `rate_limit::check_and_record`
  unchanged — the module is parameterized on the cap, so each route picks
  its own ceiling.
- A `Retry-After` cap on the mobile helper (`MAX_RETRY_AFTER_MS = 2000`) is
  the SLA for "never freeze the UI", not the server's cap; a server hint of
  60 s is honored to 2 s. If a future use-case needs longer waits (e.g.
  LLMs endpoint polling), the constant moves to a per-call option.
