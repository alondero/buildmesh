//! Bound respawns of a session that was never persisted (issue #2137).
//!
//! `prepare_context` pre-writes a fresh UUID to `agent_nodes.cli_session_id`
//! before launching (`SessionIdMode::Assign`), so the row advertises a resume
//! target the moment the spawn starts — even though the harness has not written
//! a transcript yet, and may never write one (transcript saving disabled, #2136).
//! The frontend's spawn-intent resolver (`resolveSpawnAgentIntent` in
//! `src/stores/agentNodeStore.ts`) reads that column and turns any non-empty
//! value into a `--resume <id>` request, so a node whose process died before
//! its first transcript was written is relaunched with a session id that
//! cannot resolve. Claude Code exits immediately, the row returns to `idle`,
//! and the next auto-spawn repeats it — the ~4 s loop this module bounds.
//!
//! Two guards, one owner:
//!
//! * **Drop the identity.** The first unusable resume proves the stored
//!   session id resolves to nothing, so [`note_unusable_resume`] reports
//!   [`UnusableResumeVerdict::DropIdentity`] and the caller clears
//!   `cli_session_id`. The next spawn resolves to `fresh` and starts a real
//!   conversation instead of re-issuing the same doomed `--resume`.
//! * **Bound the repeats.** If the identity comes back (a PTY re-capture, an
//!   explicit user retry, an adapter that rewrites the column) the strike count
//!   keeps rising; at [`MAX_CONSECUTIVE_UNUSABLE_RESUMES`] the caller must stop
//!   relaunching and surface the node instead. The count is dropped on that
//!   transition so a later *deliberate* user retry starts from a clean budget.
//!
//! The count is process-local (like [`crate::attention_autoclear`]) because it
//! exists only to stop an in-process loop; the durable record of a give-up is
//! the terminal status written by
//! [`crate::agent::session_lifecycle::on_resume_exhausted`].

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

/// Consecutive unusable resumes tolerated before the node is surfaced.
/// Three keeps a genuine expired-session retry (#1306) recoverable while
/// capping the loop at a handful of relaunches instead of hundreds.
pub const MAX_CONSECUTIVE_UNUSABLE_RESUMES: u32 = 3;

/// What the caller must do about a resume attempt that could not have worked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnusableResumeVerdict {
    /// The stored session id resolves to nothing. Clear `cli_session_id` so the
    /// next spawn falls back to a fresh conversation.
    DropIdentity,
    /// The bound is reached. Stop relaunching and surface the node with a
    /// reason; the budget resets so a deliberate user retry starts clean.
    Exhausted { attempts: u32 },
}

/// How long after process creation a resume attempt still counts as "this
/// session id is unusable". Wider than
/// [`super::reader::EARLY_EXIT_WINDOW`] on purpose: that window only picks the
/// `Error` vs `Idle` verdict, whereas a harness that boots, loads its config
/// and *then* discovers the conversation is gone routinely takes longer than
/// three seconds to give up. A resume that ran long enough for a user to be
/// working in it is not a never-persisted session, so this stays far below any
/// plausible working session.
pub const UNUSABLE_RESUME_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

static STRIKES: Lazy<Mutex<HashMap<i64, u32>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Whether an exit this soon after process creation marks the resume target
/// unusable. Plain terminals have no session to resume, so they never count.
pub fn is_unusable_resume(
    resume_attempt: bool,
    elapsed_since_process_creation: std::time::Duration,
) -> bool {
    resume_attempt && elapsed_since_process_creation < UNUSABLE_RESUME_WINDOW
}

/// Record one unusable resume and report what to do about it.
pub fn note_unusable_resume(node_id: i64) -> UnusableResumeVerdict {
    let mut strikes = STRIKES.lock().unwrap();
    let attempts = strikes.entry(node_id).or_insert(0);
    *attempts += 1;
    if *attempts < MAX_CONSECUTIVE_UNUSABLE_RESUMES {
        return UnusableResumeVerdict::DropIdentity;
    }
    let attempts = *attempts;
    // Give up and clear the budget: the terminal status is the durable
    // record, and a later deliberate user retry deserves a fresh budget.
    // Without this a node whose transcript came back would sit one strike
    // away from being given up on again.
    strikes.remove(&node_id);
    UnusableResumeVerdict::Exhausted { attempts }
}

/// Clear the strike count for a node — a spawn got past the point where its
/// session id could have been unusable.
pub fn reset(node_id: i64) {
    STRIKES.lock().unwrap().remove(&node_id);
}

/// Current strike count. Test seam; production code reads the verdict from
/// [`note_unusable_resume`].
#[cfg(test)]
pub(crate) fn strikes(node_id: i64) -> u32 {
    STRIKES.lock().unwrap().get(&node_id).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Node ids are unique per test: the strike map is process-global and
    // `cargo test` runs these on parallel threads.
    const A: i64 = 98_213_001;
    const B: i64 = 98_213_002;
    const C: i64 = 98_213_003;

    #[test]
    fn first_unusable_resume_drops_the_identity_rather_than_giving_up() {
        // The heart of #2137: one never-persisted session must cost exactly
        // one relaunch, not escalate the node straight to a terminal status.
        assert_eq!(note_unusable_resume(A), UnusableResumeVerdict::DropIdentity);
        assert_eq!(strikes(A), 1);
        reset(A);
    }

    #[test]
    fn repeated_unusable_resumes_stop_at_the_bound_and_surface_the_node() {
        assert_eq!(note_unusable_resume(B), UnusableResumeVerdict::DropIdentity);
        assert_eq!(note_unusable_resume(B), UnusableResumeVerdict::DropIdentity);
        assert_eq!(
            note_unusable_resume(B),
            UnusableResumeVerdict::Exhausted {
                attempts: MAX_CONSECUTIVE_UNUSABLE_RESUMES
            },
            "the bound is what keeps the relaunch loop finite"
        );
        assert_eq!(
            strikes(B),
            0,
            "giving up clears the budget for a deliberate retry"
        );
    }

    #[test]
    fn a_healthy_spawn_clears_the_strike_count() {
        assert_eq!(note_unusable_resume(C), UnusableResumeVerdict::DropIdentity);
        reset(C);
        assert_eq!(note_unusable_resume(C), UnusableResumeVerdict::DropIdentity);
        assert_eq!(
            strikes(C),
            1,
            "a reset spawn restarts the budget, not stacks onto it"
        );
        reset(C);
    }

    #[test]
    fn only_resume_attempts_that_die_immediately_count() {
        assert!(
            is_unusable_resume(true, Duration::from_millis(500)),
            "the #2137 shape: the CLI died before its first transcript"
        );
        assert!(
            is_unusable_resume(true, UNUSABLE_RESUME_WINDOW - Duration::from_millis(1)),
            "a harness that boots then finds no conversation still counts"
        );
        assert!(
            !is_unusable_resume(true, UNUSABLE_RESUME_WINDOW),
            "the window boundary itself is not an immediate exit"
        );
        assert!(
            !is_unusable_resume(false, Duration::from_millis(10)),
            "a fresh spawn (or a plain terminal) has no session id to invalidate"
        );
    }
}
