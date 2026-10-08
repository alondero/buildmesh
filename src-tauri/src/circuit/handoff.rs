//! Circuit handoff files: the full prompt and the agent's result are written to
//! disk per run and step attempt. Long terminal pastes are unreliable (collapsed,
//! scrolled, or Enter swallowed), so the full text travels through a file and
//! only a short pointer is pasted. The files are also durable per-run evidence
//! (issue #2134).

use super::delivery::VISIBLE_PASTE_TEXT_LIMIT;
use crate::models::EnvType;
use std::path::{Path, PathBuf};

/// `<root>/circuits/runs/run-<run_id>`.
pub(crate) fn run_dir_in(root: &Path, run_id: i64) -> PathBuf {
    root.join("circuits")
        .join("runs")
        .join(format!("run-{run_id}"))
}

/// The run directory under the app data dir, or `None` when no data dir is known.
// Used by the result-file slice of #2134.
#[allow(dead_code)]
pub(crate) fn run_dir(run_id: i64) -> Option<PathBuf> {
    crate::preferences::app_data_dir().map(|root| run_dir_in(&root, run_id))
}

/// The prompt and result files for one step attempt.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HandoffPaths {
    pub prompt: PathBuf,
    pub result: PathBuf,
}

/// Node ids are user-influenced, so anything outside `[A-Za-z0-9_-]` becomes `_`.
fn sanitise_node_id(node_id: &str) -> String {
    node_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn paths_in(root: &Path, run_id: i64, node_id: &str, attempt: i32) -> HandoffPaths {
    let stem = format!("{}-attempt{attempt}", sanitise_node_id(node_id));
    let dir = run_dir_in(root, run_id);
    HandoffPaths {
        prompt: dir.join(format!("{stem}.prompt.md")),
        result: dir.join(format!("{stem}.result.md")),
    }
}

/// The step attempt's handoff paths under the app data dir, or `None` when no data dir is known.
// Used by the result-file slice of #2134.
#[allow(dead_code)]
pub(crate) fn paths(run_id: i64, node_id: &str, attempt: i32) -> Option<HandoffPaths> {
    crate::preferences::app_data_dir().map(|root| paths_in(&root, run_id, node_id, attempt))
}

/// A WSL agent runs inside Linux, so it needs the `/mnt/<drive>/...` form.
/// Windows and Windows-interop agents keep the Windows path.
pub(crate) fn agent_visible_path(path: &Path, env: EnvType) -> String {
    let text = path.display().to_string();
    match env {
        EnvType::Wsl => crate::env::windows_to_wsl(&text),
        EnvType::Windows | EnvType::WindowsInterop => text,
    }
}

/// The one-line paste that stands in for a prompt too long to paste inline.
pub(crate) fn pointer_line(prompt_path: &str) -> String {
    format!("Read and follow the complete task brief in this file: {prompt_path}")
}

/// Appended to the prompt's result contract so the agent writes its report only once it is done.
pub(crate) fn result_instruction(result_path: &str) -> String {
    format!(
        "When, and only when, you have completely finished this task (including any delegated or \
         background work), write your final report, ending with the required result line, to the \
         file {result_path}, overwriting it if it exists. Do not create that file before you are done."
    )
}

/// The text to paste into the agent's terminal: the prompt itself when short
/// enough to paste inline, otherwise a pointer to its prompt file.
pub(crate) fn delivered_text(full_prompt: &str, prompt_path: &str) -> String {
    if full_prompt.chars().count() <= VISIBLE_PASTE_TEXT_LIMIT {
        full_prompt.to_string()
    } else {
        pointer_line(prompt_path)
    }
}

/// Writes through a sibling temp file and renames it over the target, so a
/// reader never sees a half-written prompt.
pub(crate) fn write_prompt(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// `Ok(None)` when the agent has not written a usable result yet (missing or blank).
pub(crate) fn read_result(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn remove_run_dir_in(root: &Path, run_id: i64) -> std::io::Result<()> {
    match std::fs::remove_dir_all(run_dir_in(root, run_id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The run ids that have a `run-<id>` folder under `<root>/circuits/runs`.
/// Only directories whose name is exactly `run-<id>` count: a stray file, or
/// `run-05` (which would otherwise round-trip to run 5's folder), is ignored.
pub(crate) fn run_ids_on_disk_in(root: &Path) -> Vec<i64> {
    let runs_dir = root.join("circuits").join("runs");
    let entries = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "circuits: could not list handoff folders in {}: {error}",
                    runs_dir.display()
                );
            }
            return Vec::new();
        }
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let run_id = name.strip_prefix("run-")?.parse::<i64>().ok()?;
            (format!("run-{run_id}") == name).then_some(run_id)
        })
        .collect()
}

/// Removes the handoff folder of every run that `existing` says is gone.
/// `existing` receives the run ids found on disk and returns the subset that
/// still exists in the database. When there are no folders it is not called.
/// If it errors, nothing is deleted: a folder is only removed on a positive answer.
/// Returns how many folders were removed.
pub(crate) fn remove_orphan_run_dirs_in(
    root: &Path,
    existing: impl FnOnce(&[i64]) -> Result<std::collections::HashSet<i64>, String>,
) -> Result<usize, String> {
    let ids = run_ids_on_disk_in(root);
    if ids.is_empty() {
        return Ok(0);
    }
    let live = existing(&ids)?;
    let mut removed = 0;
    for run_id in ids.iter().filter(|run_id| !live.contains(run_id)) {
        match remove_run_dir_in(root, *run_id) {
            Ok(()) => removed += 1,
            Err(error) => tracing::warn!(
                "circuits: could not remove the handoff folder for run {run_id}, will retry: {error}"
            ),
        }
    }
    Ok(removed)
}

/// `remove_orphan_run_dirs_in` under the app data dir. `Ok(0)` when no data dir is known.
pub(crate) fn remove_orphan_run_dirs(
    existing: impl FnOnce(&[i64]) -> Result<std::collections::HashSet<i64>, String>,
) -> Result<usize, String> {
    match crate::preferences::app_data_dir() {
        Some(root) => remove_orphan_run_dirs_in(&root, existing),
        None => Ok(0),
    }
}

/// A step attempt whose prompt asked for a result file. The turn is what the
/// agent owes: its finished turn is not complete until that file exists.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HandoffTurn {
    pub node_id: String,
    pub attempt: i32,
}

/// What was staged for one prompt: the text to paste, and the turn it opened
/// when the agent owes a result file.
pub(crate) struct StagedPrompt {
    pub delivered: String,
    pub turn: Option<HandoffTurn>,
}

/// Saves `full_prompt` as this step attempt's prompt file. Falls back to the
/// full prompt inline when the file cannot be written.
pub(crate) fn stage_prompt(
    run_id: i64,
    node_id: &str,
    attempt: i32,
    env: EnvType,
    full_prompt: &str,
    requests_result: bool,
) -> StagedPrompt {
    match crate::preferences::app_data_dir() {
        Some(root) => stage_prompt_in(
            &root,
            run_id,
            node_id,
            attempt,
            env,
            full_prompt,
            requests_result,
        ),
        None => {
            tracing::warn!(
                "circuits: run {run_id}: no app data dir, so the prompt for {node_id} attempt {attempt} is pasted inline without a result file"
            );
            StagedPrompt {
                delivered: full_prompt.to_string(),
                turn: None,
            }
        }
    }
}

/// `stage_prompt` under an explicit root. Any result file left by an earlier
/// attempt is removed first, so a stale result can never be read for a re-sent prompt.
pub(crate) fn stage_prompt_in(
    root: &Path,
    run_id: i64,
    node_id: &str,
    attempt: i32,
    env: EnvType,
    full_prompt: &str,
    requests_result: bool,
) -> StagedPrompt {
    let paths = paths_in(root, run_id, node_id, attempt);
    match std::fs::remove_file(&paths.result) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            "circuits: run {run_id}: could not remove stale result {}: {error}",
            paths.result.display()
        ),
    }
    let text = if requests_result {
        format!(
            "{full_prompt}\n\n{}",
            result_instruction(&agent_visible_path(&paths.result, env))
        )
    } else {
        full_prompt.to_string()
    };
    match write_prompt(&paths.prompt, &text) {
        Ok(()) => {
            let delivered = delivered_text(&text, &agent_visible_path(&paths.prompt, env));
            tracing::info!(
                "circuits: run {run_id}: staged prompt for {node_id} attempt {attempt} at {} (pointer: {}, result file expected: {requests_result})",
                paths.prompt.display(),
                delivered != text
            );
            StagedPrompt {
                delivered,
                turn: requests_result.then(|| HandoffTurn {
                    node_id: node_id.to_string(),
                    attempt,
                }),
            }
        }
        Err(error) => {
            // The agent still gets the result instruction, so it can save its
            // report, but nothing can be checked against a file that never existed.
            tracing::warn!(
                "circuits: run {run_id}: could not write prompt file {} for {node_id} attempt {attempt}, pasting inline: {error}",
                paths.prompt.display()
            );
            StagedPrompt {
                delivered: text,
                turn: None,
            }
        }
    }
}

/// `<run dir>/agent-<agent_node_id>.turn`: the handoff the agent's current turn belongs to.
pub(crate) fn agent_turn_marker_in(root: &Path, run_id: i64, agent_node_id: i64) -> PathBuf {
    run_dir_in(root, run_id).join(format!("agent-{agent_node_id}.turn"))
}

/// Records which handoff the agent's current turn belongs to. `None` clears the
/// marker, so a turn that expects no result file never inherits an older one.
pub(crate) fn set_agent_turn(run_id: i64, agent_node_id: i64, turn: Option<&HandoffTurn>) {
    match crate::preferences::app_data_dir() {
        Some(root) => set_agent_turn_in(&root, run_id, agent_node_id, turn),
        None if turn.is_some() => tracing::warn!(
            "circuits: run {run_id}: no app data dir, so the result file for agent {agent_node_id} is not expected"
        ),
        None => {}
    }
}

/// `set_agent_turn` under an explicit root.
pub(crate) fn set_agent_turn_in(
    root: &Path,
    run_id: i64,
    agent_node_id: i64,
    turn: Option<&HandoffTurn>,
) {
    let marker = agent_turn_marker_in(root, run_id, agent_node_id);
    let Some(turn) = turn else {
        remove_turn_marker(run_id, agent_node_id, &marker);
        return;
    };
    let text = format!("{}\n{}\n", turn.node_id, turn.attempt);
    if let Err(error) = write_prompt(&marker, &text) {
        // A marker left from an earlier turn would name the wrong result file.
        tracing::warn!(
            "circuits: run {run_id}: could not record the handoff for agent {agent_node_id} at {}: {error}",
            marker.display()
        );
        remove_turn_marker(run_id, agent_node_id, &marker);
    }
}

fn remove_turn_marker(run_id: i64, agent_node_id: i64, marker: &Path) {
    match std::fs::remove_file(marker) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            "circuits: run {run_id}: could not clear the handoff for agent {agent_node_id} at {}: {error}",
            marker.display()
        ),
    }
}

/// The result file the agent's current turn owes, or `None` when the turn expects
/// none, or its marker is unreadable.
pub(crate) fn expected_result(run_id: i64, agent_node_id: i64) -> Option<PathBuf> {
    crate::preferences::app_data_dir()
        .and_then(|root| expected_result_in(&root, run_id, agent_node_id))
}

/// `expected_result` under an explicit root.
pub(crate) fn expected_result_in(root: &Path, run_id: i64, agent_node_id: i64) -> Option<PathBuf> {
    let marker = agent_turn_marker_in(root, run_id, agent_node_id);
    let text = match std::fs::read_to_string(&marker) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                "circuits: run {run_id}: could not read the handoff for agent {agent_node_id} at {}: {error}",
                marker.display()
            );
            return None;
        }
    };
    match parse_turn_marker(&text) {
        Some(turn) => Some(paths_in(root, run_id, &turn.node_id, turn.attempt).result),
        None => {
            tracing::warn!(
                "circuits: run {run_id}: malformed handoff for agent {agent_node_id} at {}; expecting no result file",
                marker.display()
            );
            None
        }
    }
}

/// Two lines: the node id, then the attempt number.
fn parse_turn_marker(text: &str) -> Option<HandoffTurn> {
    let mut lines = text.lines();
    let node_id = lines.next().filter(|node_id| !node_id.is_empty())?;
    let attempt = lines.next()?.trim().parse::<i32>().ok()?;
    if lines.next().is_some() {
        return None;
    }
    Some(HandoffTurn {
        node_id: node_id.to_string(),
        attempt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// Process-unique scratch root: test binaries share the temp dir.
    fn scratch(suffix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "buildmesh-handoff-{}-{}",
            std::process::id(),
            suffix
        ))
    }

    #[test]
    fn run_dir_in_builds_the_documented_layout() {
        let root = Path::new("base");
        assert_eq!(
            run_dir_in(root, 343),
            Path::new("base")
                .join("circuits")
                .join("runs")
                .join("run-343")
        );
    }

    #[test]
    fn paths_in_builds_prompt_and_result_names() {
        let root = Path::new("base");
        let paths = paths_in(root, 343, "follow_feedback", 2);
        assert_eq!(
            paths.prompt.file_name(),
            Some(OsStr::new("follow_feedback-attempt2.prompt.md"))
        );
        assert_eq!(
            paths.result.file_name(),
            Some(OsStr::new("follow_feedback-attempt2.result.md"))
        );
        assert_eq!(paths.prompt.parent(), Some(run_dir_in(root, 343).as_path()));
        assert_eq!(paths.result.parent(), Some(run_dir_in(root, 343).as_path()));
    }

    #[test]
    fn paths_in_sanitises_node_ids_for_filenames() {
        let paths = paths_in(Path::new("base"), 1, "a/b c", 1);
        assert_eq!(
            paths.prompt.file_name(),
            Some(OsStr::new("a_b_c-attempt1.prompt.md"))
        );
        assert_eq!(
            paths.result.file_name(),
            Some(OsStr::new("a_b_c-attempt1.result.md"))
        );
        // Hyphens and underscores are kept; dots and unicode are replaced.
        assert_eq!(sanitise_node_id("Node-1_x.y"), "Node-1_x_y");
        assert_eq!(sanitise_node_id("né"), "n_");
    }

    #[test]
    fn delivered_text_passes_a_prompt_at_the_limit_through_unchanged() {
        let prompt = "a".repeat(256);
        assert_eq!(delivered_text(&prompt, "C:/x/p.md"), prompt);
    }

    #[test]
    fn delivered_text_replaces_a_prompt_past_the_limit_with_the_pointer() {
        let prompt = "a".repeat(257);
        assert_eq!(
            delivered_text(&prompt, "C:/x/p.md"),
            "Read and follow the complete task brief in this file: C:/x/p.md"
        );
    }

    #[test]
    fn delivered_text_counts_characters_not_bytes() {
        // 256 two-byte characters: 512 bytes, but within the 256-character limit.
        let at_limit = "é".repeat(256);
        assert_eq!(delivered_text(&at_limit, "p.md"), at_limit);
        let over_limit = "é".repeat(257);
        assert_eq!(
            delivered_text(&over_limit, "p.md"),
            "Read and follow the complete task brief in this file: p.md"
        );
    }

    #[test]
    fn pointer_line_for_a_windows_path_is_one_short_line() {
        let path = r"C:\Users\someone\AppData\Roaming\com.alond.buildmesh\circuits\runs\run-343\follow_feedback-attempt2.prompt.md";
        let line = pointer_line(path);
        assert_eq!(
            line,
            r"Read and follow the complete task brief in this file: C:\Users\someone\AppData\Roaming\com.alond.buildmesh\circuits\runs\run-343\follow_feedback-attempt2.prompt.md"
        );
        assert!(!line.contains('\n'));
        assert!(line.chars().count() < VISIBLE_PASTE_TEXT_LIMIT);
    }

    #[test]
    fn result_instruction_names_the_result_path_and_is_ascii() {
        let text = result_instruction(r"C:\x\a.result.md");
        assert!(text.contains(r"to the file C:\x\a.result.md, overwriting it if it exists"));
        assert!(text.contains("Do not create that file before you are done."));
        assert!(text.is_ascii());
    }

    #[test]
    fn write_prompt_creates_missing_parents_and_replaces_existing_content() {
        let root = scratch("write-prompt");
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("nested").join("deeper").join("x.prompt.md");

        write_prompt(&path, "first version").expect("first write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first version");

        write_prompt(&path, "second").expect("replacing write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");

        // The temp file is renamed away, not left behind.
        assert!(!path.with_file_name("x.prompt.md.tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_result_reports_missing_blank_and_present_files() {
        let root = scratch("read-result");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("a.result.md");

        assert_eq!(read_result(&path).unwrap(), None);

        std::fs::write(&path, "  \n\t\n").unwrap();
        assert_eq!(read_result(&path).unwrap(), None);

        std::fs::write(&path, "done\nBUILDMESH_RESULT_V1: pass").unwrap();
        assert_eq!(
            read_result(&path).unwrap(),
            Some("done\nBUILDMESH_RESULT_V1: pass".to_string())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_run_dir_in_removes_the_directory_and_tolerates_a_second_call() {
        let root = scratch("remove-run-dir");
        let _ = std::fs::remove_dir_all(&root);
        let paths = paths_in(&root, 7, "node", 1);
        write_prompt(&paths.prompt, "prompt").unwrap();
        assert!(run_dir_in(&root, 7).is_dir());

        remove_run_dir_in(&root, 7).expect("first removal");
        assert!(!run_dir_in(&root, 7).exists());

        remove_run_dir_in(&root, 7).expect("removing a missing run dir is Ok");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn agent_visible_path_converts_only_for_wsl_agents() {
        let path = Path::new(r"C:\x\y.md");
        assert_eq!(agent_visible_path(path, EnvType::Wsl), "/mnt/c/x/y.md");
        assert_eq!(agent_visible_path(path, EnvType::Windows), r"C:\x\y.md");
        assert_eq!(
            agent_visible_path(path, EnvType::WindowsInterop),
            r"C:\x\y.md"
        );
    }

    #[test]
    fn stage_prompt_in_pastes_a_short_prompt_inline_and_writes_it_to_the_file() {
        let root = scratch("stage-short");
        let _ = std::fs::remove_dir_all(&root);
        let paths = paths_in(&root, 3, "implement", 1);

        let staged = stage_prompt_in(
            &root,
            3,
            "implement",
            1,
            EnvType::Windows,
            "Do the thing.",
            false,
        );

        assert_eq!(staged.delivered, "Do the thing.");
        assert_eq!(staged.turn, None);
        assert_eq!(
            std::fs::read_to_string(&paths.prompt).unwrap(),
            "Do the thing."
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_prompt_in_pastes_a_long_prompt_as_a_pointer_and_keeps_the_full_text_in_the_file() {
        let root = scratch("stage-long");
        let _ = std::fs::remove_dir_all(&root);
        let paths = paths_in(&root, 3, "implement", 1);
        let full = "x".repeat(300);

        let staged = stage_prompt_in(&root, 3, "implement", 1, EnvType::Windows, &full, false);

        assert_eq!(
            staged.delivered,
            pointer_line(&paths.prompt.display().to_string())
        );
        assert_eq!(std::fs::read_to_string(&paths.prompt).unwrap(), full);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_prompt_in_removes_a_result_left_by_an_earlier_attempt() {
        let root = scratch("stage-stale-result");
        let _ = std::fs::remove_dir_all(&root);
        let paths = paths_in(&root, 4, "review", 2);
        write_prompt(&paths.result, "old report BUILDMESH_RESULT_V1: pass").unwrap();

        stage_prompt_in(
            &root,
            4,
            "review",
            2,
            EnvType::Windows,
            "Review it again.",
            true,
        );

        assert!(!paths.result.exists());
        assert_eq!(read_result(&paths.result).unwrap(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_prompt_in_leaves_other_attempts_results_alone() {
        let root = scratch("stage-other-attempt");
        let _ = std::fs::remove_dir_all(&root);
        let previous = paths_in(&root, 4, "review", 1);
        write_prompt(&previous.result, "attempt one report").unwrap();

        stage_prompt_in(
            &root,
            4,
            "review",
            2,
            EnvType::Windows,
            "Review it again.",
            false,
        );

        assert_eq!(
            std::fs::read_to_string(&previous.result).unwrap(),
            "attempt one report"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_prompt_in_falls_back_to_the_full_prompt_when_the_file_cannot_be_written() {
        let root = scratch("stage-unwritable");
        let _ = std::fs::remove_dir_all(&root);
        // The run directory's name is taken by a plain file, so creating it fails.
        std::fs::create_dir_all(root.join("circuits").join("runs")).unwrap();
        std::fs::write(run_dir_in(&root, 5), "not a directory").unwrap();
        let full = "y".repeat(300);

        let staged = stage_prompt_in(&root, 5, "implement", 1, EnvType::Windows, &full, false);

        assert_eq!(staged.delivered, full);
        assert_eq!(staged.turn, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn stage_prompt_in_points_a_wsl_agent_at_the_wsl_form_of_the_prompt_path() {
        // Scratch roots are Windows drive paths here, so the WSL agent must be
        // given the /mnt/<drive>/ form of the same file.
        let root = scratch("stage-wsl");
        let _ = std::fs::remove_dir_all(&root);
        let full = "z".repeat(300);

        let staged = stage_prompt_in(&root, 6, "implement", 1, EnvType::Wsl, &full, false);

        let prefix = "Read and follow the complete task brief in this file: /mnt/";
        assert!(
            staged.delivered.starts_with(prefix),
            "unexpected pointer: {}",
            staged.delivered
        );
        assert!(
            !staged.delivered.contains('\\'),
            "pointer leaks a Windows path: {}",
            staged.delivered
        );
        let wsl_file = staged
            .delivered
            .trim_start_matches("Read and follow the complete task brief in this file: ");
        assert_eq!(
            std::fs::read_to_string(crate::env::windows_path_from_wsl(wsl_file)).unwrap(),
            full
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_prompt_in_appends_the_result_instruction_and_opens_a_turn_when_requested() {
        let root = scratch("stage-result-requested");
        let _ = std::fs::remove_dir_all(&root);
        let paths = paths_in(&root, 8, "implement", 1);
        let result_path = paths.result.display().to_string();

        let staged = stage_prompt_in(
            &root,
            8,
            "implement",
            1,
            EnvType::Windows,
            "Implement the change.",
            true,
        );

        let file = std::fs::read_to_string(&paths.prompt).unwrap();
        assert!(
            file.starts_with("Implement the change.\n\nWhen, and only when, you have completely finished this task"),
            "unexpected prompt file: {file}"
        );
        assert!(
            file.ends_with(&format!(
                "to the file {result_path}, overwriting it if it exists. Do not create that file before you are done."
            )),
            "unexpected prompt file: {file}"
        );
        // The instruction takes the text past the paste limit, so only the pointer is pasted.
        assert_eq!(
            staged.delivered,
            pointer_line(&paths.prompt.display().to_string())
        );
        assert_eq!(
            staged.turn,
            Some(HandoffTurn {
                node_id: "implement".into(),
                attempt: 1,
            })
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_prompt_in_inlines_the_result_instruction_when_the_file_cannot_be_written() {
        let root = scratch("stage-result-unwritable");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("circuits").join("runs")).unwrap();
        std::fs::write(run_dir_in(&root, 9), "not a directory").unwrap();
        let result_path = paths_in(&root, 9, "implement", 1)
            .result
            .display()
            .to_string();

        let staged = stage_prompt_in(&root, 9, "implement", 1, EnvType::Windows, "Do it.", true);

        assert_eq!(
            staged.delivered,
            format!(
                "Do it.\n\nWhen, and only when, you have completely finished this task (including any delegated or background work), write your final report, ending with the required result line, to the file {result_path}, overwriting it if it exists. Do not create that file before you are done."
            )
        );
        // Nothing was written, so no turn can be checked against a file.
        assert_eq!(staged.turn, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn set_agent_turn_records_the_expected_result_until_cleared() {
        let root = scratch("turn-marker");
        let _ = std::fs::remove_dir_all(&root);
        let turn = HandoffTurn {
            node_id: "review".into(),
            attempt: 2,
        };

        set_agent_turn_in(&root, 10, 42, Some(&turn));

        assert_eq!(
            std::fs::read_to_string(agent_turn_marker_in(&root, 10, 42)).unwrap(),
            "review\n2\n"
        );
        assert_eq!(
            expected_result_in(&root, 10, 42),
            Some(paths_in(&root, 10, "review", 2).result)
        );

        set_agent_turn_in(&root, 10, 42, None);
        assert!(!agent_turn_marker_in(&root, 10, 42).exists());
        assert_eq!(expected_result_in(&root, 10, 42), None);
        // Clearing a turn that has no marker is not an error.
        set_agent_turn_in(&root, 10, 42, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_turn_marker_expects_no_result_file() {
        let root = scratch("turn-marker-malformed");
        let _ = std::fs::remove_dir_all(&root);
        let marker = agent_turn_marker_in(&root, 11, 43);

        write_prompt(&marker, "review\nnot-a-number\n").unwrap();
        assert_eq!(expected_result_in(&root, 11, 43), None);

        write_prompt(&marker, "\n2\n").unwrap();
        assert_eq!(expected_result_in(&root, 11, 43), None);

        write_prompt(&marker, "review\n2\nstray line\n").unwrap();
        assert_eq!(expected_result_in(&root, 11, 43), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_ids_on_disk_in_takes_only_run_named_directories() {
        let root = scratch("ids-on-disk");
        let _ = std::fs::remove_dir_all(&root);
        let runs = root.join("circuits").join("runs");
        std::fs::create_dir_all(runs.join("run-1")).unwrap();
        std::fs::create_dir_all(runs.join("run-22")).unwrap();
        std::fs::create_dir_all(runs.join("run-x")).unwrap();
        std::fs::create_dir_all(runs.join("run-05")).unwrap();
        std::fs::write(runs.join("notes.txt"), "not a run").unwrap();
        // A run folder name taken by a plain file is not a run folder.
        std::fs::write(runs.join("run-3"), "file").unwrap();

        let mut ids = run_ids_on_disk_in(&root);
        ids.sort();
        assert_eq!(ids, vec![1, 22]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_ids_on_disk_in_is_empty_when_the_runs_dir_is_missing() {
        let root = scratch("ids-missing");
        let _ = std::fs::remove_dir_all(&root);
        assert!(run_ids_on_disk_in(&root).is_empty());
    }

    #[test]
    fn remove_orphan_run_dirs_in_removes_only_runs_the_database_no_longer_has() {
        let root = scratch("orphans-removed");
        let _ = std::fs::remove_dir_all(&root);
        for run_id in [1, 2, 3] {
            write_prompt(&paths_in(&root, run_id, "node", 1).prompt, "prompt").unwrap();
        }

        let removed = remove_orphan_run_dirs_in(&root, |ids| {
            let mut sorted = ids.to_vec();
            sorted.sort();
            assert_eq!(sorted, vec![1, 2, 3]);
            Ok([2].into_iter().collect())
        })
        .expect("reconciliation succeeds");

        assert_eq!(removed, 2);
        assert!(!run_dir_in(&root, 1).exists());
        assert!(run_dir_in(&root, 2).is_dir());
        assert!(!run_dir_in(&root, 3).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_orphan_run_dirs_in_deletes_nothing_when_the_existence_check_fails() {
        let root = scratch("orphans-error");
        let _ = std::fs::remove_dir_all(&root);
        write_prompt(&paths_in(&root, 4, "node", 1).prompt, "prompt").unwrap();

        let result = remove_orphan_run_dirs_in(&root, |_| Err("database busy".to_string()));

        assert_eq!(result, Err("database busy".to_string()));
        assert!(run_dir_in(&root, 4).is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_orphan_run_dirs_in_does_not_ask_the_database_when_there_are_no_folders() {
        let root = scratch("orphans-none");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("circuits").join("runs")).unwrap();
        let mut asked = false;

        let removed = remove_orphan_run_dirs_in(&root, |_| {
            asked = true;
            Ok(std::collections::HashSet::new())
        });

        assert_eq!(removed, Ok(0));
        assert!(!asked, "existence check must not run without run folders");
        let _ = std::fs::remove_dir_all(&root);
    }
}
