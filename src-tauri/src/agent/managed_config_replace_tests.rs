//! Regression coverage for the replace refusal in [`super::atomic_write`]
//! (issue #2626).
//!
//! The flake this covers is
//! `agent::managed_config::tests::concurrent_writes_do_not_share_a_temp_name`:
//! several writers replaced the same target at once, and on Windows one of
//! them lost that race with `ERROR_ACCESS_DENIED` — the destination is locked
//! for as long as the other writer's replace is in flight. The fix retries
//! the replace while the destination is locked.
//!
//! Each case here drives that retry without a race: the policy tests inject
//! the rename, the wait and the lock predicate, and the two Windows cases
//! hold a real destination open and let the refusal itself release it. No
//! case waits on a clock to decide what happened.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

/// The Windows refusal the retry exists for: `ERROR_ACCESS_DENIED`.
const LOCKED: i32 = 5;

/// An error that is not the lock refusal: `ENOENT`.
const NOT_LOCKED: i32 = 2;

/// The lock predicate the production code uses on Windows, mirrored here so
/// the retry *policy* is testable on any platform. The classification itself
/// is Windows-only and is covered by
/// `the_production_predicate_clears_only_the_destination_lock_refusals`.
fn lock_refusal(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(LOCKED)
}

/// A policy with no jitter, so the waits a test observes are exact.
fn exact_policy(attempts: u32) -> ReplacePolicy {
    ReplacePolicy {
        attempts,
        backoff: Duration::from_millis(1),
        jitter: Duration::ZERO,
    }
}

/// The names of the files in `dir`, sorted.
fn file_names(dir: &TempDir) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_locked_destination_is_retried_until_the_replace_lands() {
    let attempts = AtomicUsize::new(0);
    let waits = AtomicUsize::new(0);
    let mut rename = || {
        if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            Err(std::io::Error::from_raw_os_error(LOCKED))
        } else {
            Ok(())
        }
    };
    let result = replace_with_retry(
        &exact_policy(5),
        |_: Duration| {
            waits.fetch_add(1, Ordering::SeqCst);
        },
        &mut rename,
        lock_refusal,
    );
    assert!(result.is_ok(), "the replace must land once the lock clears");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(waits.load(Ordering::SeqCst), 2);
}

#[test]
fn a_replace_that_is_not_a_lock_refusal_is_not_retried() {
    let attempts = AtomicUsize::new(0);
    let waits = AtomicUsize::new(0);
    let mut rename = || {
        attempts.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::from_raw_os_error(NOT_LOCKED))
    };
    let error = replace_with_retry(
        &exact_policy(5),
        |_: Duration| {
            waits.fetch_add(1, Ordering::SeqCst);
        },
        &mut rename,
        lock_refusal,
    )
    .expect_err("a refusal the retry does not recognise must be returned");
    assert_eq!(error.raw_os_error(), Some(NOT_LOCKED));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(waits.load(Ordering::SeqCst), 0);
}

#[test]
fn the_retry_budget_is_bounded_and_returns_the_lock_error() {
    let attempts = AtomicUsize::new(0);
    let waits = AtomicUsize::new(0);
    let mut rename = || {
        attempts.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::from_raw_os_error(LOCKED))
    };
    let error = replace_with_retry(
        &exact_policy(3),
        |_: Duration| {
            waits.fetch_add(1, Ordering::SeqCst);
        },
        &mut rename,
        lock_refusal,
    )
    .expect_err("a destination that stays locked must not spin forever");
    assert_eq!(error.raw_os_error(), Some(LOCKED));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(waits.load(Ordering::SeqCst), 2);
}

#[test]
fn the_production_budget_is_spent_and_the_lock_error_is_reported() {
    let attempts = AtomicUsize::new(0);
    let waits = AtomicUsize::new(0);
    let mut rename = || {
        attempts.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::from_raw_os_error(LOCKED))
    };
    let error = replace_with_retry(
        &REPLACE_POLICY,
        |_: Duration| {
            waits.fetch_add(1, Ordering::SeqCst);
        },
        &mut rename,
        lock_refusal,
    )
    .expect_err("a destination that stays locked must not spin forever");
    assert_eq!(error.raw_os_error(), Some(LOCKED));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        REPLACE_POLICY.attempts as usize
    );
    assert_eq!(
        waits.load(Ordering::SeqCst),
        REPLACE_POLICY.attempts as usize - 1
    );
}

/// The production wait carries a jittered share, so writers that lost the
/// same race do not wake up together and collide again.
#[test]
fn the_production_wait_carries_a_jittered_share() {
    assert!(
        REPLACE_POLICY.jitter > Duration::ZERO,
        "a fixed wait would let every loser wake at the same moment"
    );
    let waits = std::sync::Mutex::new(Vec::new());
    let mut rename = || Err(std::io::Error::from_raw_os_error(LOCKED));
    let error = replace_with_retry(
        &REPLACE_POLICY,
        |wait: Duration| waits.lock().unwrap().push(wait),
        &mut rename,
        lock_refusal,
    )
    .expect_err("a destination that stays locked must not spin forever");
    assert_eq!(error.raw_os_error(), Some(LOCKED));
    let waits = waits.lock().unwrap();
    assert!(!waits.is_empty());
    for wait in waits.iter() {
        assert!(
            *wait >= REPLACE_POLICY.backoff
                && *wait < REPLACE_POLICY.backoff + REPLACE_POLICY.jitter,
            "the wait must be the backoff plus a share of the jitter, got {wait:?}"
        );
    }
}

#[test]
fn the_production_retry_window_outlasts_a_contended_replace() {
    // The window has to outlast the milliseconds a contended replace holds
    // the destination, so the budget is a duration and not a bare count.
    let window = REPLACE_POLICY.backoff * (REPLACE_POLICY.attempts - 1);
    assert!(
        window >= Duration::from_millis(100),
        "the retry window must outlast a contended replace"
    );
}

#[test]
fn a_write_to_a_new_path_lands_without_leaving_a_temp_file() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("hooks.json");
    atomic_write(&target, "payload").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "payload");
    assert_eq!(file_names(&dir), vec!["hooks.json".to_string()]);
}

#[test]
fn a_failed_write_leaves_no_temp_file_behind() {
    let dir = TempDir::new().unwrap();
    // A directory stands where the target file should be, so the rename does
    // not land.
    let target = dir.path().join("hooks.json");
    std::fs::create_dir(&target).unwrap();
    assert!(
        atomic_write(&target, "payload").is_err(),
        "a write over a directory must fail"
    );
    assert_eq!(
        file_names(&dir),
        vec!["hooks.json".to_string()],
        "the failed write must clean its temp file up"
    );
}

/// The refusal itself: a destination another writer holds makes Windows
/// refuse the replace, the refusal leaves the source in place, and a retry
/// lands it once the destination is released.
///
/// The production predicate is the one under test here, so this also proves
/// that a real `ERROR_ACCESS_DENIED` is recognised as transient.
#[cfg(windows)]
#[test]
fn a_refused_replace_leaves_the_source_in_place_and_a_retry_lands_it() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::mpsc;

    let dir = TempDir::new().unwrap();
    let target = dir.path().join("hooks.json");
    let source = dir.path().join("source.tmp");
    std::fs::write(&target, "old").unwrap();
    std::fs::write(&source, "new").unwrap();

    // An exclusive handle on the destination is what a concurrent replace
    // looks like to `MoveFileExW`: the replace is refused with
    // `ERROR_ACCESS_DENIED` for as long as the handle is open.
    let hold = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&target)
        .unwrap();

    let (refused_tx, refused_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let writer_source = source.clone();
    let writer_target = target.clone();
    let writer = std::thread::spawn(move || {
        replace_with_retry(
            &REPLACE_POLICY,
            |_: Duration| {
                // The driver only waits when it was refused, so this signal is
                // proof that the locked destination refused the replace.
                let _ = refused_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            },
            || std::fs::rename(&writer_source, &writer_target),
            destination_is_locked,
        )
    });

    refused_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a locked destination must refuse the replace");
    assert!(
        source.exists(),
        "a refused replace must leave the source in place, or the retry could lose the payload"
    );
    drop(hold);
    release_tx.send(()).unwrap();

    let result = writer.join().unwrap();
    assert!(
        result.is_ok(),
        "the replace must land once the destination is released: {result:?}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
}

/// The production path: a write against a destination another writer holds
/// lands once the destination is released.
///
/// The refusal releases the destination itself, so the retry that follows is
/// the thing that lands the write — no test thread races the clock to find
/// the moment the rename was refused.
#[cfg(windows)]
#[test]
fn a_write_over_a_locked_destination_lands_once_the_lock_is_released() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::Mutex;

    let dir = TempDir::new().unwrap();
    let target = dir.path().join("hooks.json");
    std::fs::write(&target, "old").unwrap();

    // An exclusive handle on the destination is what a concurrent replace
    // looks like to `MoveFileExW`: the replace is refused with
    // `ERROR_ACCESS_DENIED` for as long as the handle is open.
    let hold = Mutex::new(Some(
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&target)
            .unwrap(),
    ));

    let writer_target = target.clone();
    let writer = std::thread::spawn(move || {
        atomic_write_with_replace(&writer_target, "new", |temp, path| {
            let result = std::fs::rename(temp, path);
            if result.is_err() {
                // The refusal is what releases the destination, so the retry
                // that follows is the thing that lands the write.
                drop(hold.lock().unwrap().take());
            }
            result
        })
    });

    let result = writer.join().unwrap();
    assert!(
        result.is_ok(),
        "the write must land once the destination is released: {result:?}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    assert_eq!(file_names(&dir), vec!["hooks.json".to_string()]);
}

/// The production predicate clears the destination-lock refusals and leaves
/// the rest alone.
#[cfg(windows)]
#[test]
fn the_production_predicate_clears_only_the_destination_lock_refusals() {
    for code in [5, 32, 33] {
        assert!(
            destination_is_locked(&std::io::Error::from_raw_os_error(code)),
            "the destination-lock refusal {code} must be retried"
        );
    }
    for code in [2, 13] {
        assert!(
            !destination_is_locked(&std::io::Error::from_raw_os_error(code)),
            "a refusal that is not a destination lock must not be retried"
        );
    }
}
