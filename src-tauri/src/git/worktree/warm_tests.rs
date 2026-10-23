// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;

#[test]
fn manual_claim_keeps_a_detached_entry_where_it_is() {
    assert_eq!(
        warm_checkout_plan(DETACHED_MODE, None),
        WarmCheckout::LeaveAsIs
    );
}

#[test]
fn manual_claim_creates_the_branch_for_every_other_mode() {
    // The pre-#2533 rule: a Manual claim branches unless the mode is
    // `detached`.
    assert_eq!(
        warm_checkout_plan(BRANCHED_MODE, None),
        WarmCheckout::BranchAtHead
    );
    assert_eq!(warm_checkout_plan("", None), WarmCheckout::BranchAtHead);
}

#[test]
fn issue_pr_claim_lands_on_the_target_sha() {
    assert_eq!(
        warm_checkout_plan(BRANCHED_MODE, Some("abc123")),
        WarmCheckout::BranchAt("abc123".to_string())
    );
    assert_eq!(
        warm_checkout_plan(DETACHED_MODE, Some("abc123")),
        WarmCheckout::DetachAt("abc123".to_string())
    );
    // Any other mode detaches, as the pre-#2533 checkout did.
    assert_eq!(
        warm_checkout_plan("", Some("abc123")),
        WarmCheckout::DetachAt("abc123".to_string())
    );
}

#[test]
fn freshness_requires_a_matching_recorded_sha() {
    assert!(warm_entry_is_fresh(Some("abc123"), "abc123"));
    assert!(!warm_entry_is_fresh(Some("abc123"), "def456"));
    // A missing recorded SHA is stale: the pool has no proof that the entry
    // sits on the required commit.
    assert!(!warm_entry_is_fresh(None, "abc123"));
}
