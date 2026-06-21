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

static TICKETS: OnceLock<RwLock<HashMap<String, Instant>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, Instant>> {
    TICKETS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Mint a fresh single-use ticket for an already-authenticated request. Returns
/// the opaque value the client passes as `?ticket=`. Prunes expired entries on
/// the way in, so the table stays bounded by (mint rate × TTL) without a
/// background sweep.
pub fn mint() -> String {
    mint_at(Instant::now())
}

fn mint_at(now: Instant) -> String {
    let ticket = crate::db::generate_token();
    let mut tickets = store().write();
    tickets.retain(|_, &mut issued| now.duration_since(issued) < TICKET_TTL);
    tickets.insert(ticket.clone(), now);
    ticket
}

/// Consume a ticket: present, unexpired, and removed (single-use) → `true`. A
/// missing, empty, expired, or already-used ticket → `false`.
pub fn consume(ticket: &str) -> bool {
    if ticket.is_empty() {
        return false;
    }
    let mut tickets = store().write();
    match tickets.remove(ticket) {
        Some(issued) => Instant::now().duration_since(issued) < TICKET_TTL,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ticket_consumes_exactly_once() {
        let t = mint();
        assert!(consume(&t), "a freshly minted ticket must validate");
        assert!(!consume(&t), "a ticket is single-use — the second consume fails");
    }

    #[test]
    fn unknown_and_empty_tickets_are_rejected() {
        assert!(!consume("never-minted"));
        assert!(!consume(""));
    }

    #[test]
    fn expired_ticket_is_rejected() {
        // Mint with an issued time well in the past so it is already expired by
        // the time we consume it.
        let past = Instant::now()
            .checked_sub(TICKET_TTL * 2)
            .expect("test clock underflow");
        let t = mint_at(past);
        assert!(!consume(&t), "an expired ticket must not validate");
    }

    #[test]
    fn mint_prunes_expired_entries() {
        let past = Instant::now()
            .checked_sub(TICKET_TTL * 2)
            .expect("test clock underflow");
        let stale = mint_at(past);
        // Minting fresh prunes the stale entry, so it is no longer even present.
        let _fresh = mint();
        assert!(
            !store().read().contains_key(&stale),
            "mint() should prune expired tickets"
        );
    }
}
