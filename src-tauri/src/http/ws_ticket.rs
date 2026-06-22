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
use serde::Serialize;
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

/// How long a minted ticket stays valid. A ticket is minted then immediately
/// used for the upgrade on the same screen, so this only needs to cover network
/// latency; a short window keeps the replay surface tiny.
const TICKET_TTL: Duration = Duration::from_secs(30);

/// Each ticket remembers when it was issued and which device session minted it
/// (`None` for the root token, which owns no device row and can't be revoked).
/// The device binding is what lets a later revocation find and kick the live
/// WebSocket the device opened with this ticket (issue #502).
type TicketEntry = (Instant, Option<i64>);

static TICKETS: OnceLock<RwLock<HashMap<String, TicketEntry>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, TicketEntry>> {
    TICKETS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Mint a fresh single-use ticket for an already-authenticated request, bound to
/// the minting `device_id` (issue #502; `None` for the root token). Returns the
/// opaque value the client passes as `?ticket=`. Prunes expired entries on the
/// way in, so the table stays bounded by (mint rate × TTL) without a background
/// sweep.
pub fn mint(device_id: Option<i64>) -> String {
    mint_at(Instant::now(), device_id)
}

fn mint_at(now: Instant, device_id: Option<i64>) -> String {
    let ticket = crate::db::generate_token();
    let mut tickets = store().write();
    tickets.retain(|_, &mut (issued, _)| now.duration_since(issued) < TICKET_TTL);
    tickets.insert(ticket.clone(), (now, device_id));
    ticket
}

/// Consume a ticket: present, unexpired, and removed (single-use) → `Some(device
/// id)` (the bound device, or `None` for a root-token ticket). A missing, empty,
/// expired, or already-used ticket → `None`. The outer `Option` is validity; the
/// inner is the device binding — so a valid root-token ticket reads as
/// `Some(None)`.
pub fn consume(ticket: &str) -> Option<Option<i64>> {
    if ticket.is_empty() {
        return None;
    }
    let mut tickets = store().write();
    match tickets.remove(ticket) {
        Some((issued, device_id)) if Instant::now().duration_since(issued) < TICKET_TTL => {
            Some(device_id)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ticket_consumes_exactly_once() {
        let t = mint(None);
        assert_eq!(consume(&t), Some(None), "a freshly minted ticket must validate");
        assert_eq!(consume(&t), None, "a ticket is single-use — the second consume fails");
    }

    #[test]
    fn ticket_carries_its_minting_device_id() {
        // The device binding is what a revocation later matches against to kick
        // the right live WebSocket (issue #502).
        let t = mint(Some(42));
        assert_eq!(consume(&t), Some(Some(42)));
    }

    #[test]
    fn unknown_and_empty_tickets_are_rejected() {
        assert_eq!(consume("never-minted"), None);
        assert_eq!(consume(""), None);
    }

    #[test]
    fn expired_ticket_is_rejected() {
        // Mint with an issued time well in the past so it is already expired by
        // the time we consume it.
        let past = Instant::now()
            .checked_sub(TICKET_TTL * 2)
            .expect("test clock underflow");
        let t = mint_at(past, None);
        assert_eq!(consume(&t), None, "an expired ticket must not validate");
    }

    #[test]
    fn mint_prunes_expired_entries() {
        let past = Instant::now()
            .checked_sub(TICKET_TTL * 2)
            .expect("test clock underflow");
        let stale = mint_at(past, None);
        // Minting fresh prunes the stale entry, so it is no longer even present.
        let _fresh = mint(None);
        assert!(
            !store().read().contains_key(&stale),
            "mint() should prune expired tickets"
        );
    }
}
