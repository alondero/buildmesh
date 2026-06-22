//! Short-lived, single-use WebSocket handshake tickets (issue #500, AC4).
//!
//! A browser cannot set an `Authorization` header on a WebSocket upgrade, and
//! proxies routinely strip cookies on it — which is why the old handshake
//! carried the long-lived token in the `?token=` URL. That is exactly the leak
//! #500 removes. The replacement: an already-authenticated request
//! (`POST /api/ws-ticket`) mints a one-time ticket here, and the client passes
//! it as `?ticket=` on the upgrade. Because a ticket can only be obtained
//! through a cookie/header-protected fetch, a cross-site page cannot mint one —
//! closing the cross-site WebSocket hijacking hole a raw cookie-on-WS would
//! leave open. Tickets are single-use and expire after [`TICKET_TTL`].

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The body of a successful `POST /api/ws-ticket` — the one-time ticket the
/// mobile client appends as `?ticket=` to its WebSocket URL. Derived (not
/// hand-written in TS) per the repo's shared-types rule; the mobile client
/// imports the generated `WsTicket` type.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "WsTicket.ts")]
pub struct WsTicket {
    pub ticket: String,
}

/// The WebSocket surface a ticket may open: the per-node terminal stream, or the
/// node-less events push. The mint request names one and the upgrade is checked
/// against it (issue #551), so a ticket for the `events` push can't be replayed
/// onto a node's `terminal` and vice versa.
pub const SURFACE_TERMINAL: &str = "terminal";
pub const SURFACE_EVENTS: &str = "events";

/// What a ticket is bound to (issue #551): the WebSocket surface, and — for the
/// node-scoped `terminal` surface — the specific node. Derived (not hand-written
/// in TS) per the repo's shared-types rule; the mobile client sends this as the
/// `POST /api/ws-ticket` body and the upgrade handler reconstructs the requested
/// target from the URL to compare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "WsTarget.ts")]
pub struct WsTarget {
    /// `"terminal"` or `"events"` ([`SURFACE_TERMINAL`] / [`SURFACE_EVENTS`]).
    pub surface: String,
    /// The node the `terminal` surface targets; `None` for `events`.
    #[ts(as = "Option<i32>")]
    pub node_id: Option<i64>,
}

impl WsTarget {
    /// A target a client may legitimately ask for: a known surface, with a
    /// `node_id` exactly when the surface is node-scoped. A malformed request
    /// (unknown surface, or `terminal` without a node) is rejected at mint with
    /// `400` rather than minting a ticket that could never match an upgrade.
    pub fn is_well_formed(&self) -> bool {
        match self.surface.as_str() {
            SURFACE_TERMINAL => self.node_id.is_some(),
            SURFACE_EVENTS => self.node_id.is_none(),
            _ => false,
        }
    }
}

/// Parse and validate a `POST /api/ws-ticket` body into the target to bind
/// (issue #551). `Err(())` is the caller's signal to respond `400 Bad Request`:
/// an absent body, malformed JSON, or a target that is not [well-formed]
/// (unknown surface, or a `node_id` that doesn't match the surface) all collapse
/// to the same client error — we never mint a ticket that could never match an
/// upgrade.
///
/// [well-formed]: WsTarget::is_well_formed
pub fn parse_mint_target(body: &[u8]) -> Result<WsTarget, ()> {
    let target: WsTarget = serde_json::from_slice(body).map_err(|_| ())?;
    if target.is_well_formed() {
        Ok(target)
    } else {
        Err(())
    }
}

/// Outcome of presenting a ticket on a WebSocket upgrade (issue #551).
#[derive(Debug, PartialEq, Eq)]
pub enum ConsumeOutcome {
    /// Valid, unexpired, and bound to the requested target → consumed
    /// (single-use). Carries the minting device id (`None` for the root token)
    /// so a later revocation can kick this socket (issue #502).
    Ok(Option<i64>),
    /// Missing, empty, expired, or already-used → `401 Unauthorized`.
    Invalid,
    /// Present and unexpired, but bound to a *different* target → `403
    /// Forbidden`. The ticket is **left in place**, so a misrouted legitimate
    /// client can retry against its real target within the TTL.
    TargetMismatch,
}

/// How long a minted ticket stays valid. A ticket is minted then immediately
/// used for the upgrade on the same screen, so this only needs to cover network
/// latency; a short window keeps the replay surface tiny.
const TICKET_TTL: Duration = Duration::from_secs(30);

/// Each ticket remembers when it was issued, which device session minted it
/// (`None` for the root token, which owns no device row and can't be revoked),
/// and the target it is bound to (issue #551). The device binding is what lets a
/// later revocation find and kick the live WebSocket the device opened with this
/// ticket (issue #502); the target binding is what narrows a leaked ticket to
/// the single surface/node the caller asked for.
type TicketEntry = (Instant, Option<i64>, WsTarget);

static TICKETS: OnceLock<RwLock<HashMap<String, TicketEntry>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, TicketEntry>> {
    TICKETS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Mint a fresh single-use ticket for an already-authenticated request, bound to
/// the minting `device_id` (issue #502; `None` for the root token) and the
/// `target` the caller will open (issue #551). Returns the opaque value the
/// client passes as `?ticket=`. Prunes expired entries on the way in, so the
/// table stays bounded by (mint rate × TTL) without a background sweep.
pub fn mint(device_id: Option<i64>, target: WsTarget) -> String {
    mint_at(Instant::now(), device_id, target)
}

fn mint_at(now: Instant, device_id: Option<i64>, target: WsTarget) -> String {
    let ticket = crate::db::generate_token();
    let mut tickets = store().write();
    tickets.retain(|_, (issued, _, _)| now.duration_since(*issued) < TICKET_TTL);
    tickets.insert(ticket.clone(), (now, device_id, target));
    ticket
}

/// Consume a ticket against the `requested` target the upgrade is for (issue
/// #551):
/// - present, unexpired, and bound to `requested` → [`ConsumeOutcome::Ok`] with
///   the device binding, and the ticket is removed (single-use);
/// - present and unexpired but bound to a different target →
///   [`ConsumeOutcome::TargetMismatch`], and the ticket is **left in place** so
///   the real client can still use it;
/// - missing, empty, expired, or already-used → [`ConsumeOutcome::Invalid`].
pub fn consume(ticket: &str, requested: &WsTarget) -> ConsumeOutcome {
    if ticket.is_empty() {
        return ConsumeOutcome::Invalid;
    }
    let mut tickets = store().write();
    let Some((issued, device_id, target)) = tickets.get(ticket) else {
        return ConsumeOutcome::Invalid;
    };
    if Instant::now().duration_since(*issued) >= TICKET_TTL {
        // Expired: drop it and treat as invalid. (mint() also prunes, but a
        // consume can be the first touch after expiry.)
        tickets.remove(ticket);
        return ConsumeOutcome::Invalid;
    }
    if target != requested {
        // Wrong target — do NOT consume, so a misrouted legitimate client can
        // retry against its real target within the TTL (issue #551).
        return ConsumeOutcome::TargetMismatch;
    }
    let device_id = *device_id;
    tickets.remove(ticket);
    ConsumeOutcome::Ok(device_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(node_id: i64) -> WsTarget {
        WsTarget { surface: SURFACE_TERMINAL.to_string(), node_id: Some(node_id) }
    }

    fn events() -> WsTarget {
        WsTarget { surface: SURFACE_EVENTS.to_string(), node_id: None }
    }

    #[test]
    fn minted_ticket_consumes_exactly_once_on_its_target() {
        let t = mint(None, terminal(7));
        assert_eq!(
            consume(&t, &terminal(7)),
            ConsumeOutcome::Ok(None),
            "a freshly minted ticket must validate on its bound target"
        );
        assert_eq!(
            consume(&t, &terminal(7)),
            ConsumeOutcome::Invalid,
            "a ticket is single-use — the second consume fails"
        );
    }

    #[test]
    fn ticket_carries_its_minting_device_id() {
        // The device binding is what a revocation later matches against to kick
        // the right live WebSocket (issue #502).
        let t = mint(Some(42), terminal(1));
        assert_eq!(consume(&t, &terminal(1)), ConsumeOutcome::Ok(Some(42)));
    }

    #[test]
    fn wrong_node_is_rejected_and_ticket_stays_valid() {
        // Issue #551: a leaked ticket presented on a different node is 403, and
        // crucially is NOT consumed — so the real client can still use it.
        let t = mint(None, terminal(7));
        assert_eq!(
            consume(&t, &terminal(99)),
            ConsumeOutcome::TargetMismatch,
            "a ticket must not open a node it wasn't bound to"
        );
        assert_eq!(
            consume(&t, &terminal(7)),
            ConsumeOutcome::Ok(None),
            "the mismatch must NOT have consumed the ticket"
        );
    }

    #[test]
    fn wrong_surface_on_same_node_is_rejected() {
        // Issue #551: surface is part of the binding — an `events` ticket can't
        // be replayed onto a terminal even with a matching (here, absent) node.
        let t = mint(None, events());
        assert_eq!(
            consume(&t, &terminal(1)),
            ConsumeOutcome::TargetMismatch,
            "an events ticket must not open a terminal"
        );
        // And the terminal ticket can't be used for the events push.
        let t2 = mint(None, terminal(1));
        assert_eq!(consume(&t2, &events()), ConsumeOutcome::TargetMismatch);
    }

    #[test]
    fn unknown_and_empty_tickets_are_rejected() {
        assert_eq!(consume("never-minted", &events()), ConsumeOutcome::Invalid);
        assert_eq!(consume("", &events()), ConsumeOutcome::Invalid);
    }

    #[test]
    fn expired_ticket_is_rejected() {
        // Mint with an issued time well in the past so it is already expired by
        // the time we consume it.
        let past = Instant::now()
            .checked_sub(TICKET_TTL * 2)
            .expect("test clock underflow");
        let t = mint_at(past, None, terminal(1));
        assert_eq!(
            consume(&t, &terminal(1)),
            ConsumeOutcome::Invalid,
            "an expired ticket must not validate"
        );
    }

    #[test]
    fn mint_prunes_expired_entries() {
        let past = Instant::now()
            .checked_sub(TICKET_TTL * 2)
            .expect("test clock underflow");
        let stale = mint_at(past, None, terminal(1));
        // Minting fresh prunes the stale entry, so it is no longer even present.
        let _fresh = mint(None, terminal(2));
        assert!(
            !store().read().contains_key(&stale),
            "mint() should prune expired tickets"
        );
    }

    #[test]
    fn parse_mint_target_accepts_well_formed_bodies() {
        assert_eq!(
            parse_mint_target(br#"{"surface":"terminal","node_id":7}"#),
            Ok(terminal(7))
        );
        assert_eq!(
            parse_mint_target(br#"{"surface":"events","node_id":null}"#),
            Ok(events())
        );
    }

    #[test]
    fn parse_mint_target_rejects_missing_or_malformed_target() {
        // AC: a missing target on mint is a 400. An empty body (Content-Length 0
        // → empty slice) is the on-the-wire shape of "no target".
        assert_eq!(parse_mint_target(b""), Err(()));
        assert_eq!(parse_mint_target(b"not json"), Err(()));
        // Missing the required `surface` field.
        assert_eq!(parse_mint_target(br#"{"node_id":7}"#), Err(()));
        // Well-typed JSON but not a legitimate target: terminal needs a node…
        assert_eq!(parse_mint_target(br#"{"surface":"terminal","node_id":null}"#), Err(()));
        // …and an unknown surface is refused outright.
        assert_eq!(parse_mint_target(br#"{"surface":"control","node_id":7}"#), Err(()));
    }

    #[test]
    fn well_formed_rules_match_surfaces() {
        assert!(terminal(1).is_well_formed());
        assert!(events().is_well_formed());
        // terminal requires a node…
        assert!(!WsTarget { surface: SURFACE_TERMINAL.into(), node_id: None }.is_well_formed());
        // …events must not carry one…
        assert!(!WsTarget { surface: SURFACE_EVENTS.into(), node_id: Some(1) }.is_well_formed());
        // …and an unknown surface is never well-formed.
        assert!(!WsTarget { surface: "control".into(), node_id: Some(1) }.is_well_formed());
    }
}
