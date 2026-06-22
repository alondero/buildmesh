//! Device-session revocation signal (issue #502).
//!
//! Deleting a device's row stops the *next* request from authenticating, but an
//! already-open WebSocket never re-checks auth — it would keep streaming a
//! node's PTY to a phone the user just revoked. To honour "revoked session
//! tokens immediately terminate active HTTP/WS requests", the revoke path fires
//! the revoked device id onto this broadcast channel; every live WebSocket
//! handler subscribes and closes its socket the instant it sees its own id.
//!
//! Same `tokio::sync::broadcast` shape as [`super::events`], but the payload is
//! a bare `device_id` and there is no WebSocket endpoint — it's a purely
//! server-internal kick signal.

use std::sync::OnceLock;
use tokio::sync::broadcast;

static REVOCATIONS: OnceLock<broadcast::Sender<i64>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<i64> {
    REVOCATIONS.get_or_init(|| {
        // Capacity 256: if a handler lags past this (a burst of revocations while
        // it's busy forwarding PTY output) its `recv()` returns `Lagged` rather
        // than a specific id. `revocation_terminates` deliberately treats `Lagged`
        // as "close this device socket" — it can't tell whether its own id was in
        // the dropped window, so it errs toward closing; a still-valid device just
        // reconnects and re-authenticates. So a revocation is never *missed*, at
        // the cost of an occasional precautionary disconnect under heavy churn.
        let (tx, _) = broadcast::channel(256);
        tx
    })
}

/// Subscribe to revocation signals. Each live WebSocket handler holds one
/// receiver and breaks its read loop when a matching id arrives.
pub fn subscribe() -> broadcast::Receiver<i64> {
    channel().subscribe()
}

/// Announce that `device_id` was revoked, kicking any of its live sockets.
/// Fire-and-forget: with no live sockets the send returns `Err`, which we
/// ignore (the deleted row already blocks every future request).
pub fn revoke(device_id: i64) {
    let _ = channel().send(device_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriber_receives_the_revoked_id() {
        let mut rx = subscribe();
        revoke(99);
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        assert_eq!(got, 99);
    }
}
