//! MiniMax 0.6.13 persists failed tasks here even when their last transcript
//! poll raced termination and no background-tasks/<id>/summary.txt was written.
//! Only a native task owned by the canonical session may settle its launch.

use super::{is_terminal_task_status, LaunchedTask, TaskStoreFact};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};

/// Both completion observers use the stores beside their canonical session.
pub(super) struct StorePaths {
    pub tasks_dir: PathBuf,
    pub database: PathBuf,
}

impl StorePaths {
    pub(super) fn for_transcript(transcript: &Path) -> Option<Self> {
        let data_dir = transcript.ancestors().find_map(|ancestor| {
            (ancestor.file_name()? == "sessions" && ancestor.parent()?.file_name()? == "v2")
                .then(|| ancestor.parent()?.parent())
                .flatten()
        })?;
        Some(Self {
            tasks_dir: data_dir.join("background-tasks"),
            database: data_dir.join("v2/sqlite/runtime-state.sqlite"),
        })
    }
}

pub(super) fn reconcile(
    transcript: &Path,
    database: &Path,
    launched: &[LaunchedTask],
    facts: &mut Vec<TaskStoreFact>,
) -> Option<()> {
    if launched.is_empty() {
        return Some(());
    }
    match std::fs::metadata(database) {
        Ok(metadata) if metadata.is_file() => {}
        // Older installations can still prove termination through receipts or
        // summary files. Only absence permits the legacy fallback.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(()),
        Ok(_) | Err(_) => return None,
    }
    let session = super::manifest_session_id(&transcript.with_file_name("manifest.json"))?;
    let conn = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(100))
        .ok()?;
    let mut statement = conn
        .prepare(
            "SELECT status, created_at_ms, ended_at_ms FROM local_runtime_background_tasks
                  WHERE task_id=?1 AND owner_session_id=?2",
        )
        .ok()?;
    for task in launched {
        let row = statement
            .query_row(rusqlite::params![task.task_id, session], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .optional()
            .ok()?;
        let Some((status, created_at_ms, ended_at_ms)) = row else {
            continue;
        };
        let finished = is_terminal_task_status(&status)
            && ended_at_ms.is_some_and(|ended| ended >= created_at_ms);
        // A current running/unknown row must veto older receipt/file evidence.
        facts.retain(|fact| fact.task_id != task.task_id);
        facts.push(TaskStoreFact {
            task_id: task.task_id.clone(),
            settled_at_ms: ended_at_ms,
            finished,
        });
    }
    Some(())
}

#[cfg(test)]
mod task_store_tests;
