//! Desktop-issued, single-use invitations. Only hashes live in memory; restarting
//! or disabling LAN invalidates pending invitations, never paired devices.
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

const TTL: Duration = Duration::from_secs(300);

#[derive(Default)]
struct Invitations(Vec<(String, Instant)>);

impl Invitations {
    fn mint(&mut self, now: Instant) -> String {
        self.0.retain(|(_, expiry)| *expiry > now);
        // Bound desktop remounts without invalidating a QR still on screen.
        if self.0.len() >= 8 {
            self.0.remove(0);
        }
        let ticket = crate::db::generate_token();
        self.0.push((crate::db::hash_token(&ticket), now + TTL));
        ticket
    }

    fn take(&mut self, ticket: &str, now: Instant) -> Option<(String, Instant)> {
        self.0.retain(|(_, expiry)| *expiry > now);
        let hash = crate::db::hash_token(ticket);
        match self.0.iter().position(|(stored, _)| *stored == hash) {
            Some(index) => Some(self.0.remove(index)),
            None => None,
        }
    }

    fn restore(&mut self, entry: (String, Instant), now: Instant) {
        if entry.1 > now {
            self.0.push(entry);
        }
    }
}

fn invitations() -> &'static Mutex<Invitations> {
    static STORE: OnceLock<Mutex<Invitations>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Invitations::default()))
}

pub fn mint() -> String {
    invitations().lock().mint(Instant::now())
}

pub fn invalidate() {
    invitations().lock().0.clear();
}

pub fn exchange(ticket: &str, label: Option<&str>, ip: &str) -> rusqlite::Result<Option<String>> {
    // Reserve under one lock before creating the device: parallel exchanges
    // cannot mint two identities. If persistence fails, restore the reservation
    // through its original expiry so a transient DB error does not burn the QR.
    let reservation = invitations().lock().take(ticket, Instant::now());
    let Some(reservation) = reservation else {
        return Ok(None);
    };
    let conn = crate::db::write_conn();
    let result = crate::db::pair_device_session_inner(&conn, label, Some(ip))
        .map(|(_, token)| Some(token));
    if result.is_err() {
        invitations().lock().restore(reservation, Instant::now());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_are_hashed_expire_and_cannot_be_replayed() {
        let mut store = Invitations::default();
        let now = Instant::now();
        let ticket = store.mint(now);
        assert_ne!(store.0[0].0, ticket);
        assert!(store.take("wrong", now).is_none());
        assert!(store.take(&ticket, now).is_some());
        assert!(store.take(&ticket, now).is_none());
        let expired = store.mint(now);
        assert!(store.take(&expired, now + TTL).is_none());
    }

    #[test]
    fn a_failed_exchange_can_restore_the_unexpired_reservation() {
        let mut store = Invitations::default();
        let now = Instant::now();
        let ticket = store.mint(now);
        let reservation = store.take(&ticket, now).unwrap();
        assert!(store.take(&ticket, now).is_none());
        store.restore(reservation, now);
        assert!(store.take(&ticket, now).is_some());
    }

    #[test]
    fn concurrent_exchange_has_one_winner() {
        let store = std::sync::Arc::new(Mutex::new(Invitations::default()));
        let ticket = store.lock().mint(Instant::now());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (store, ticket, barrier) = (store.clone(), ticket.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    store.lock().take(&ticket, Instant::now()).is_some()
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|t| t.join().ok())
                .filter(|won| *won)
                .count(),
            1
        );
    }
}
