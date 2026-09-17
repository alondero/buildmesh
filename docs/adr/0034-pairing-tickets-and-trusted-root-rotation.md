# 34. One-Time Pairing Tickets and Trusted-Root Rotation

Status: accepted

## Context

Remote access needs to authorize a browser on a trusted local network without
putting a reusable administrator credential in a URL, browser history, or
server log. The original remote-access PRD and early device-session ADRs
describe a shared root-token exchange; the running app now has a narrower
pairing contract and a certificate lifecycle that must be recorded as the
current behavior.

## Decision

1. **Pairing is a one-time invitation.** The desktop creates a pairing ticket
   that expires after five minutes and can be consumed once. The QR code puts
   the ticket in the URL fragment (`#pair=...`), so it is not sent as an HTTP
   query parameter. A fresh pairing is required after expiry or consumption.
2. **The pairing endpoint mints a device session.** The mobile client sends the
   ticket to `POST /api/pair`. The server consumes it, creates the device
   session, and sets the browser's HttpOnly session cookie. The ticket is not a
   standing API credential and must not be copied into support reports.
3. **Device identity is independently revocable.** Authorized Devices lists
   paired sessions. Revoking one device invalidates its HTTP session and closes
   its active WebSocket connections; other devices remain paired.
4. **Transport depends on the listener.** Loopback remains plain HTTP for local
   agent callbacks. LAN/VPN interfaces use HTTPS/WSS with a Buildmesh-trusted
   self-signed root and leaf certificate. The desktop exposes the root
   certificate through the mobile install flow; resetting certificates
   invalidates trust and requires installation again.
5. **Exposure is explicit and opt-in.** LAN/VPN exposure is disabled by
   default. The status surface reports the interfaces that are actually
   reachable, and disabling exposure closes active remote connections.

## Alternatives considered

- **Reusable root token in the QR URL:** rejected because a copied QR image or
  browser history would retain a standing administrator credential.
- **Query-string authentication for WebSockets:** rejected because URLs can
  leak through logs, history, and referrer metadata.
- **Plain HTTP on LAN:** rejected because phone-to-desktop credentials and
  terminal traffic would be exposed to other local-network participants.
- **Public internet listener:** rejected; remote access remains a trusted-LAN
  or user-managed VPN capability rather than an internet-facing service.

## Consequences

- A QR screenshot or stale invitation has a short, one-use window rather than
  being a reusable administrator secret.
- Users must install the generated root certificate on a new phone before a
  browser can establish trusted HTTPS/WSS connections.
- The session cookie is the browser credential after pairing; the ticket is not
  a supported replacement for an authenticated API key.
- Loopback HTTP remains intentionally separate from LAN/VPN transport so local
  harness hooks do not need a certificate.
- The supported user workflow is documented in the [remote access guide](../user-guide.md#remote-access).

## Not covered

This decision does not make the coordinator read API a phone API. That surface
has its own capability-scoped token and remains separately disabled by default.
It also does not provide public internet exposure; users need an authenticated
tunnel or VPN they control for access outside a trusted local network.
