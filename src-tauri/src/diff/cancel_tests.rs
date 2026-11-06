//! The cancellation registry's suite — moved with the registry
//! (issue #2527).
//!
//! The test names are the ones the suite had inside `commands/diff.rs`; the
//! module they live in moved, the assertions did not.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::diff::cancel::{
    acquire_diff_cancel, current_cancel_token, release_diff_cancel,
};

/// Acquiring a fresh token returns a non-cancelled flag, so the
/// first invocation of a `node_id` walks the full diff.
#[test]
fn acquire_diff_cancel_yields_fresh_flag() {
    let node_id = 91001_i64;
    let token = acquire_diff_cancel(node_id);
    assert!(
        !token.load(Ordering::SeqCst),
        "freshly acquired token must not be cancelled"
    );
    release_diff_cancel(node_id, &token);
}

/// The whole point of the per-node map: a second `acquire_diff_cancel`
/// for the same `node_id` flips the first token, so the prior
/// `run_blocking` closure exits at its next poll. Pool pressure
/// for this `node_id` collapses to one in-flight task regardless
/// of the frontend's fetch rate.
#[test]
fn acquire_diff_cancel_cancels_prior_token_for_same_node() {
    let node_id = 91002_i64;
    let token_a = acquire_diff_cancel(node_id);
    let token_b = acquire_diff_cancel(node_id);

    assert!(
        token_a.load(Ordering::SeqCst),
        "a second acquire must flip the prior flag so the stale task exits"
    );
    assert!(
        !token_b.load(Ordering::SeqCst),
        "the freshly acquired token must be unset"
    );

    release_diff_cancel(node_id, &token_b);
    release_diff_cancel(node_id, &token_a);
}

/// `release_diff_cancel` only clears the map entry when its token
/// is still the current one. If a fresh acquisition has already
/// replaced the token, releasing the old one is a no-op — stomping
/// would clobber the new arrival's flag and silently break the
/// very contract above.
#[test]
fn release_after_replace_is_a_no_op() {
    let node_id = 91003_i64;
    let token_a = acquire_diff_cancel(node_id);
    let token_b = acquire_diff_cancel(node_id);

    // Release the *old* token after it's been replaced.
    release_diff_cancel(node_id, &token_a);

    // The new token must remain the current entry, unchanged.
    let current = current_cancel_token(node_id).expect("the current token is registered");
    assert!(
        Arc::ptr_eq(&current, &token_b),
        "release of a stale token must not displace the current entry"
    );

    release_diff_cancel(node_id, &token_b);
}
