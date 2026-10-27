//! Durable native hook receipts and prompt-to-turn binding; locked helpers reuse one connection.

use super::*;
pub(crate) fn receive_native_hook(
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> Result<(), DbError> {
    let mut db = crate::db::write_conn();
    receive_native_hook_locked(&mut db, receipt)
}

/// A Buildmesh prompt submission awaiting a harness acknowledgement
/// (issue #1898).
///
/// `submission_seq` is a per-agent-node ordinal assigned inside the writing
/// transaction. It is the *ordering* half of the submission proof: a receipt
/// may only claim the newest submission, so a turn whose start hook arrived
/// after Buildmesh had already submitted again can never acknowledge the
/// later one. It is stored in the record rather than in process memory
/// because the record is written *before* the PTY write — the harness can
/// report a prompt within milliseconds of Enter, and there must be no window
/// in which Buildmesh has written to the terminal but has no record to match
/// the report against.
///
/// The prompt text is deliberately absent: only `prompt_digest` is kept, and
/// the text is never written to the ledger.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PromptSubmission {
    pub agent_node_id: i64,
    pub submission_seq: i64,
    pub prompt_digest: String,
}

/// Record a Buildmesh prompt submission for a Circuit step. Callers must
/// invoke this **before** writing the prompt into the PTY so the record
/// exists by the time the harness can echo the prompt back.
pub(crate) fn record_prompt_submission(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    prompt: &crate::circuit::delivery::PreparedPrompt,
) -> Result<i64, DbError> {
    let db = crate::db::write_conn();
    record_prompt_submission_locked(
        &db,
        run_id,
        node_id,
        attempt,
        prompt.node_id(),
        prompt.text(),
    )
}

pub(crate) fn record_prompt_submission_locked(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent_node_id: i64,
    prompt: &str,
) -> Result<i64, DbError> {
    // The ordinal is per agent node, not per run: the fence that matters is
    // "has Buildmesh submitted to this terminal again", which is a property
    // of the PTY rather than of any one run. Allocating it under the same
    // writer transaction as the insert keeps concurrent runs from minting
    // the same ordinal.
    let previous = newest_submission_seq(db, agent_node_id)?;
    let submission = PromptSubmission {
        agent_node_id,
        submission_seq: previous + 1,
        prompt_digest: crate::services::circuit_worker::native_hooks::submission_digest(prompt),
    };
    append_history(
        db,
        HistoryEvent {
            run_id,
            node_id: Some(node_id),
            attempt: Some(attempt),
            kind: "prompt_submitted",
            detail: &serde_json::to_string(&submission)?,
            source: Some(SOURCE_PROMPT_SUBMISSION),
            disposition: Some(DISPOSITION_RECORDED),
        },
    )?;
    // This history entry advances the same revision used by transition fences.
    // Return it under the writer lock so the dispatcher's own write cannot
    // invalidate its subsequent PromptDelivered transition.
    Ok(revision_inner(db, run_id)?)
}

/// The SQL `newest_submission_seq` runs, kept as a constant so the plan test
/// in this module pins the statement production actually executes.
///
/// SQLite matches an expression index against the expression the query uses,
/// so the `json_extract(detail,'$.agent_node_id')` term here has to stay the
/// same expression `idx_circuit_history_submission` indexes. A changed JSON
/// path or a different function stops the match, which puts the full
/// `circuit_run_history` scan back on this hot path (issue #1932).
pub(super) const NEWEST_SUBMISSION_SEQ_SQL: &str =
    "SELECT COALESCE(MAX(json_extract(detail,'$.submission_seq')),0)
         FROM circuit_run_history
         WHERE kind='prompt_submitted' AND json_extract(detail,'$.agent_node_id')=?1";

pub(super) fn newest_submission_seq(db: &Connection, agent_node_id: i64) -> SqlResult<i64> {
    db.query_row(NEWEST_SUBMISSION_SEQ_SQL, params![agent_node_id], |row| {
        row.get(0)
    })
}

/// What a receipt established about the Buildmesh submission its turn belongs
/// to: the input stamp the binding was made against, and the ordinal of the
/// recorded submission that earned it. Both are `None` when nothing was
/// provably bound.
///
/// The two halves travel together through every path. A terminal receipt that
/// inherited the stamp but dropped the ordinal would break the ledger's own
/// contract — it could no longer answer "which input completed here?" without
/// re-deriving it from the turn start (issue #1898 review).
pub(super) type SubmissionBinding = (Option<String>, Option<i64>);

/// A wait may start after the prompt's start hook already arrived for its
/// upstream delivery step. Inherit only the newest submission for this agent,
/// from a retained ancestor attempt; sibling and retired attempts stay fenced.
pub(super) fn upstream_submission_owner(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    agent: i64,
) -> SqlResult<Option<(String, i32)>> {
    let newest = newest_submission_seq(db, agent)?;
    let mut query = db.prepare("SELECT h.node_id,h.attempt FROM circuit_run_history h
        JOIN autopilot_circuit_run_steps s ON s.run_id=h.run_id AND s.node_id=h.node_id AND s.attempt=h.attempt
        WHERE h.run_id=?1 AND h.kind='prompt_submitted' AND json_extract(h.detail,'$.agent_node_id')=?2
        AND json_extract(h.detail,'$.submission_seq')=?3 ORDER BY h.id DESC")?;
    let rows = query.query_map(params![run_id, agent, newest], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?))
    })?;
    let graph =
        run_graph(db, run_id).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    for row in rows {
        let (owner, owner_attempt) = row?;
        // Attempts are local to a step. A cycle cannot authorize a different
        // attempt of this same step through the ancestor branch.
        let eligible = if owner == node_id {
            owner_attempt == attempt
        } else {
            graph.is_ancestor(node_id, &owner)
        };
        if eligible {
            return Ok(Some((owner, owner_attempt)));
        }
    }
    Ok(None)
}

/// The binding a previously recorded, correlated turn start established for
/// this receipt's turn token. `Stop` carries the token but not the prompt
/// text, so it can only inherit a binding made when the turn began.
pub(super) fn recorded_turn_binding(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> SqlResult<Option<SubmissionBinding>> {
    let local = recorded_turn_binding_at(db, run_id, node_id, attempt, receipt)?;
    if local.is_some() {
        return Ok(local);
    }
    let Some((owner, owner_attempt)) =
        upstream_submission_owner(db, run_id, node_id, attempt, receipt.agent_node_id)?
    else {
        return Ok(None);
    };
    recorded_turn_binding_at(db, run_id, &owner, owner_attempt, receipt)
}

pub(super) fn recorded_turn_binding_at(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> SqlResult<Option<SubmissionBinding>> {
    db.query_row(
        "SELECT json_extract(detail,'$.input_stamp'),
                json_extract(detail,'$.submission_seq')
         FROM circuit_run_history
         WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='native_hook_received'
         AND json_extract(detail,'$.agent_node_id')=?4
         AND json_extract(detail,'$.session_incarnation') IS ?5
         AND json_extract(detail,'$.hook.session_id') IS ?6
         AND json_extract(detail,'$.hook.turn_id') IS ?7
         AND json_extract(detail,'$.hook.event')='UserPromptSubmit'
         AND json_extract(detail,'$.submission_correlated')=1
         ORDER BY id LIMIT 1",
        params![
            run_id,
            node_id,
            attempt,
            receipt.agent_node_id,
            receipt.session_incarnation,
            receipt.hook.session_id,
            receipt.hook.turn_id
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// Decide whether a `UserPromptSubmit` receipt provably acknowledges a
/// recorded Buildmesh submission (issue #1898).
///
/// `UserPromptSubmit` is the only event carrying both halves of Claude Code's
/// native submission contract: the verbatim `prompt` text the harness says it
/// received, and the `prompt_id` turn token naming the turn it is starting.
/// A turn token alone proves nothing about *which* Buildmesh input it belongs
/// to, so the token is bound only when all of the following hold inside one
/// (run, step, attempt, agent) scope:
///
/// 1. The receipt is stamped with a current input stamp, so Buildmesh can
///    still tell which submission is live.
/// 2. The harness's prompt digest equals a recorded submission's digest — the
///    harness received byte-for-byte the text Buildmesh wrote.
/// 3. That submission is the newest one for this agent, so no later
///    Buildmesh submission has been made since. This is what stops a delayed
///    start hook from claiming the submission that replaced it.
/// 4. No *other* turn has already claimed that same submission, so two turns
///    can never both acknowledge one submission while a later submission
///    still gets its own turn.
///
/// Returns `None` for every refusal. The receipt is still recorded, replayed
/// and presented — as reduced-confidence evidence that cannot complete a
/// step. That is the explicit unavailable path: a missing `prompt_id`
/// (Claude Code before v2.1.196), a missing or transformed `prompt`, a
/// superseded submission, or an ambiguous claim all land here.
pub(super) fn earn_turn_binding(
    db: &Connection,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> SqlResult<Option<SubmissionBinding>> {
    let hook = &receipt.hook;
    let (Some(turn_id), Some(digest)) = (hook.turn_id.as_deref(), hook.prompt_digest.as_deref())
    else {
        return Ok(None);
    };
    let Some(stamp) = receipt.input_stamp.clone() else {
        return Ok(None);
    };
    let Some((owner, owner_attempt)) =
        upstream_submission_owner(db, run_id, node_id, attempt, receipt.agent_node_id)?
    else {
        return Ok(None);
    };
    let node_id = owner.as_str();
    let attempt = owner_attempt;
    // Rule 2.
    let submission: Option<PromptSubmission> = db
        .query_row(
            "SELECT detail FROM circuit_run_history
             WHERE run_id=?1 AND node_id=?2 AND attempt=?3 AND kind='prompt_submitted'
             AND json_extract(detail,'$.agent_node_id')=?4
             AND json_extract(detail,'$.prompt_digest')=?5
             ORDER BY json_extract(detail,'$.submission_seq') DESC LIMIT 1",
            params![run_id, node_id, attempt, receipt.agent_node_id, digest],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|detail| serde_json::from_str(&detail).ok());
    let Some(submission) = submission else {
        return Ok(None);
    };
    // Rule 3.
    if submission.submission_seq != newest_submission_seq(db, receipt.agent_node_id)? {
        return Ok(None);
    }
    // Rule 4, plus idempotency: read the turns that have already claimed
    // *this* submission, under the same session generation. A prior
    // generation's claim cannot block a re-submission into a restarted
    // session, and a later submission starts with an empty claim set.
    let claimed: Vec<String> = {
        let mut stmt = db.prepare(
            "SELECT json_extract(detail,'$.hook.turn_id') FROM circuit_run_history
             WHERE run_id=?1 AND kind='native_hook_received'
             AND json_extract(detail,'$.agent_node_id')=?2
             AND json_extract(detail,'$.session_incarnation') IS ?3
             AND json_extract(detail,'$.hook.event')='UserPromptSubmit'
             AND json_extract(detail,'$.submission_correlated')=1
             AND json_extract(detail,'$.submission_seq')=?4",
        )?;
        let rows = stmt.query_map(
            params![
                run_id,
                receipt.agent_node_id,
                receipt.session_incarnation,
                submission.submission_seq
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<SqlResult<Vec<_>>>()?
    };
    if !claimed.iter().any(|turn| turn == turn_id) && !claimed.is_empty() {
        return Ok(None);
    }
    Ok(Some((Some(stamp), Some(submission.submission_seq))))
}

pub(crate) fn receive_native_hook_locked(
    db: &mut Connection,
    receipt: &crate::services::circuit_worker::native_hooks::NativeReceipt,
) -> Result<(), DbError> {
    let tx = db.transaction()?;
    let run_ids = {
        let mut stmt = tx.prepare("SELECT id FROM autopilot_circuit_runs r WHERE r.state IN ('running','paused')
            AND (r.source_agent_node_id=?1 OR EXISTS (SELECT 1 FROM autopilot_circuit_run_steps s WHERE s.run_id=r.id AND s.agent_node_id=?1))")?;
        let rows = stmt.query_map([receipt.agent_node_id], |r| r.get::<_, i64>(0))?;
        rows.collect::<SqlResult<Vec<_>>>()?
    };
    let mut targets = Vec::new();
    for run_id in run_ids {
        use crate::circuit::{
            model::StepOutcome,
            stepper::{RunState, RunView, StepStatus, StepView},
        };
        let run = crate::db::circuit::ledger::get_circuit_run_inner(&tx, run_id)?
            .ok_or_else(|| DbError::not_found("Run no longer exists"))?;
        let rows = crate::db::circuit::ledger::list_circuit_run_steps_inner(&tx, run_id)?;
        let view = RunView {
            run_id,
            state: RunState::from_db_str(&run.state),
            graph: run_graph(&tx, run_id)?,
            context: super::super::artifacts::load_context(&tx, run_id)?,
            steps: rows
                .into_iter()
                .map(|s| StepView {
                    node_id: s.node_id,
                    attempt: s.attempt,
                    status: StepStatus::from_db_str(&s.status),
                    outcome: s.outcome.as_deref().and_then(StepOutcome::from_db_str),
                    error: s.error_message,
                    agent_node_id: s.agent_node_id,
                })
                .collect(),
        };
        for step in &view.steps {
            if matches!(step.status, StepStatus::Running | StepStatus::Unverified)
                && step
                    .agent_node_id
                    .or_else(|| view.resolve_target_agent(&step.node_id))
                    == Some(receipt.agent_node_id)
            {
                targets.push((run_id, step.node_id.clone(), step.attempt));
            }
        }
    }
    for (run_id, node_id, attempt) in targets {
        let mut receipt = receipt.clone();
        // Receipt arrival can follow a newer PTY submission, so a terminal
        // hook may only claim input through the persisted turn-start binding
        // (issue #1898). A `UserPromptSubmit` receipt is the only place a
        // turn token is ever earned, and only from the harness prompt echo
        // plus submission ordering — never from arrival order.
        let binding: Option<SubmissionBinding> = if receipt.hook.event == "UserPromptSubmit" {
            earn_turn_binding(&tx, run_id, &node_id, attempt, &receipt)?
        } else {
            recorded_turn_binding(&tx, run_id, &node_id, attempt, &receipt)?
        };
        match binding {
            Some((stamp, seq)) => {
                receipt.submission_correlated = stamp.is_some();
                receipt.input_stamp = stamp;
                // A terminal receipt records the same ordinal its turn start
                // bound, so the ledger answers "which input completed here?"
                // without re-deriving it (issue #1898 review).
                receipt.submission_seq = seq;
            }
            None => {
                // Uncorrelated: drop the receipt-time input stamp entirely so
                // no downstream freshness check can read authority from it.
                receipt.submission_correlated = false;
                receipt.input_stamp = None;
                receipt.submission_seq = None;
            }
        }
        let detail = serde_json::to_string(&receipt)?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM circuit_run_history WHERE run_id=?1 AND node_id=?2 AND attempt=?3
            AND kind='native_hook_received' AND json_extract(detail,'$.source_id')=?4)",
            params![run_id,node_id,attempt,receipt.source_id], |r| r.get(0))?;
        if !exists {
            append_history(
                &tx,
                HistoryEvent {
                    run_id,
                    node_id: Some(&node_id),
                    attempt: Some(attempt),
                    kind: "native_hook_received",
                    detail: &detail,
                    source: Some(SOURCE_NATIVE_HOOK),
                    disposition: Some(DISPOSITION_RECEIVED),
                },
            )?;
        }
    }
    Ok(tx.commit()?)
}

pub(crate) fn native_hook_receipts(run_id: i64, after: i64) -> SqlResult<Vec<CircuitHistoryEntry>> {
    let db = crate::db::read_conn()?;
    let mut stmt = db.prepare("SELECT id,node_id,attempt,kind,detail,source,disposition,observed_at FROM circuit_run_history WHERE run_id=?1 AND id>?2 AND kind='native_hook_received' ORDER BY id LIMIT 512")?;
    let rows = stmt.query_map(params![run_id, after], |r| {
        Ok(CircuitHistoryEntry {
            id: r.get(0)?,
            node_id: r.get(1)?,
            attempt: r.get(2)?,
            kind: r.get(3)?,
            detail: r.get(4)?,
            source: r.get(5)?,
            disposition: r.get(6)?,
            observed_at: r.get(7)?,
        })
    })?;
    rows.collect()
}
