//! Builds the Archive list: the on-disk harness sessions Buildmesh may not
//! track yet, plus the durable archived rows it already tracks (issue #1065).
//! The Claude-Code JSONL primitives (path encoding, synthetic-injection
//! skipping, content-text extraction) live in `transcript_reader` so this
//! discovery scan and the coordinator's transcript reader share one source of
//! Claude-Code-format truth (ADR-0008). A format change breaks in one place.
//!
//! Two kinds of entry reach the caller, and the difference is what the
//! `resumable` flag on [`ArchivedAgentNode`] carries:
//!
//! * **Discovered** — a transcript exists on disk. Resume works.
//! * **Durable** — an `agent_nodes` row Buildmesh archived, whose transcript
//!   is gone or was never written (an archived Terminal node has no CLI
//!   session id at all). The row, its worktree and its branch still exist, so
//!   it belongs in the Archive list even though there is nothing to resume;
//!   `resumable: false` tells the surfaces to say so instead of offering a
//!   Resume that silently does nothing.

use crate::db;
use crate::env;
use crate::models::EnvType;
use crate::models::Provider;
use crate::services::commandcode_session;
use crate::services::transcript_paths::{encode_path, first_text_block, is_synthetic_message};
use crate::services::transcript_reader::adapters::commandcode::commandcode_project_slug;
use crate::services::transcript_reader::adapters::cursor::cursor_workspace_slug;
use crate::services::transcript_reader::types::truncate;
use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use ts_rs::TS;

/// An entry in the Archive list. The desktop Tauri `discover_agent_nodes`
/// command and the mobile HTTP route both serialise this struct. The
/// `session_id` field is Claude Code's CLI identifier and stays as-is per
/// CONTEXT.md ambiguity #1.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "ArchivedAgentNode.ts")]
pub struct ArchivedAgentNode {
    pub session_id: String,
    pub first_message: String,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub timestamp: Option<String>,
    pub worktree_name: Option<String>,
    /// Whether a transcript for this entry was found on disk, i.e. whether
    /// Resume can do anything. `false` on a durable archived row with no
    /// `cli_session_id`, or one whose transcript was deleted (issue #1065).
    /// The surfaces must not offer Resume on a `false` entry.
    pub resumable: bool,
    /// The harness that owns this conversation — an adapter id from
    /// [`Provider`] (`anthropic`, `cursor`, `commandcode`, `agy`, …), stamped
    /// by the scanner that read the transcript, or carried over from the
    /// durable row's own `agent_nodes.provider` (issue #2332).
    ///
    /// This is provenance, not a preference: it is what the executor must be
    /// for Resume to land in the right conversation. A UUID and a readable
    /// transcript cannot tell a consumer which harness wrote them, so the
    /// importer must never guess one. `None` means *unknown* — a durable row
    /// with no recorded provider, or a scanner that does not yet stamp one —
    /// and the import boundary rejects it instead of defaulting to Anthropic.
    pub provider: Option<String>,
    /// The durable `agent_nodes.id` behind this entry when there is one: an
    /// archived row the list is keeping visible. Present on durable entries
    /// and on a discovered entry that reconciles to an archived row with the
    /// same `cli_session_id`, so a caller can tell reopening known work from
    /// importing external work (issue #2332). Reopen itself is a later
    /// lifecycle step; today this is the reference, not an action.
    // A plain `#[ts(as = "i32")]` would emit `number` and drop the null case:
    // `as` replaces the whole field type, and this field is an `Option` that
    // serde sends as `null`. `type` takes the union directly.
    #[ts(type = "number | null")]
    pub node_id: Option<i64>,
}

/// Extract the worktree name from a Claude-Code encoded project directory
/// name (issue #1519). `effective_encoded` is `encode_path` of the mesh's
/// effective worktree container, so
/// `-Users-adam-repo--custom-wt-fancy-name` → `Some("fancy-name")` for a
/// `custom-wt` container. `mesh_encoded` is the
/// fallback for sessions cut before a directory change (or hand-placed
/// under a built-in container): only the exact built-in markers count there,
/// so a root-level subdir session like `...-repo-src-tauri` still maps to
/// `None` instead of a phantom worktree.
fn extract_worktree_name(
    dir_name: &str,
    mesh_encoded: &str,
    effective_encoded: &str,
) -> Option<String> {
    if !effective_encoded.is_empty() {
        if let Some(name) = dir_name
            .strip_prefix(&format!("{effective_encoded}-"))
            .filter(|s| !s.is_empty())
        {
            return Some(name.to_string());
        }
    }
    if !mesh_encoded.is_empty() {
        let suffix = dir_name.strip_prefix(mesh_encoded)?;
        for marker in ["--worktrees-", "--claude-worktrees-"] {
            if let Some(name) = suffix.strip_prefix(marker).filter(|s| !s.is_empty()) {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// True when a Command Code project dir belongs to a mesh: exactly the mesh
/// slug (root sessions), under the effective container slug (issue #1519 —
/// `{eff}-...`, covering relative custom dirs and absolute locations
/// outside the root), or under a built-in worktree marker (sessions cut
/// before a directory change).
/// Sibling repos sharing a slug prefix (`api` vs `api-gateway`) do not match
/// (the trailing `-` in each worktree prefix is load-bearing), and an empty
/// slug matches nothing (it would otherwise prefix-match every project dir
/// on disk). Case-insensitive: slugs are lowercase but the host filesystem
/// may not be.
fn is_commandcode_project_dir_for_mesh(
    dir_name: &str,
    encoded_prefix: &str,
    effective_slug: &str,
) -> bool {
    if encoded_prefix.is_empty() {
        return false;
    }
    let dir_lower = dir_name.to_lowercase();
    if dir_lower == encoded_prefix {
        return true;
    }
    if !effective_slug.is_empty()
        && dir_lower.starts_with(&format!("{}-", effective_slug.to_lowercase()))
    {
        return true;
    }
    ["worktrees", "claude-worktrees"]
        .iter()
        .any(|marker| dir_lower.starts_with(&format!("{encoded_prefix}-{marker}-")))
}

/// Strip XML/HTML-like tags from a string for clean display.
fn strip_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut inside_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => result.push(ch),
            _ => {}
        }
    }
    result.trim().to_string()
}

/// Parse a JSONL session file to extract the first real user message and metadata.
/// Skips synthetic injected messages (e.g. local-command-caveat) and reads until
/// it finds a genuine user-authored entry.
#[allow(clippy::type_complexity)]
fn parse_session_file(
    path: &PathBuf,
) -> Option<(String, Option<String>, Option<String>, Option<String>)> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        let Some(val) = serde_json::from_str::<serde_json::Value>(&line).ok() else {
            continue;
        };

        if val.get("type").and_then(|t| t.as_str()) == Some("user") {
            if let Some(msg) = val.get("message") {
                if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
                    continue;
                }
                // A discovered-session title is the opening of the FIRST text
                // block, kept single-line: joining all blocks (the transcript
                // reader's `concat_text_blocks`) would splice a multi-block user
                // message into a multi-line title that `strip_tags` won't
                // collapse before the 80-char truncate (issue #335).
                let text = first_text_block(msg.get("content"));

                if is_synthetic_message(&text) {
                    continue;
                }

                let clean = strip_tags(&text);
                let display = truncate(&clean, 80);
                if display.is_empty() {
                    continue;
                }
                let branch = val
                    .get("gitBranch")
                    .and_then(|b| b.as_str())
                    .map(|s| s.to_string());
                let cwd = val
                    .get("cwd")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string());
                let timestamp = val
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string());

                return Some((display, branch, cwd, timestamp));
            }
        }
    }
    None
}

/// Build the Archive list for a mesh: every harness session found on disk that
/// no live node already tracks, plus every durable archived row the disk scan
/// could not surface (issue #1065).
///
/// The DB read happens once, up front: the same rows both exclude session ids
/// a live node already owns (`tracked_ids`) and supply the durable archived
/// entries. No filesystem I/O runs while the connection is checked out — the
/// row set is an owned `Vec` before the scanners start.
///
/// Issue #1519: the mesh row is loaded by id (indexed primary key — path
/// strings suffer drive-letter casing and slash discrepancies on Windows)
/// to resolve the effective worktree container once; every scanner below
/// receives it instead of re-deriving layout. A *missing* row degrades to
/// legacy-only matching rather than failing the whole scan.
///
/// Issue #2330: the tracked-node read below is authoritative and no longer
/// degrades. It used to `unwrap_or_default()`, so a failed read produced an
/// empty `tracked_ids` set — every session already owned by a live node became
/// eligible for import (creating duplicates of work that is running), and
/// every durable archived row disappeared from the Archive list. Disk scanning
/// cannot substitute for ownership persistence, so an unreadable ownership row
/// is now an error.
pub fn discover(
    mesh_id: i64,
    mesh_path: &str,
) -> Result<Vec<ArchivedAgentNode>, crate::read_outcome::Unavailable> {
    let claude_dir = env::claude_dir();
    let projects_dir = claude_dir.join("projects");

    // One read serves both roles: session ids already owned by a live node
    // (which must not be re-surfaced as adoptable discoveries) and the
    // archived rows the Archive list has to keep showing. This read is
    // authoritative — it is the only record of who owns what — so it must
    // succeed before any disk scan runs (issue #2330).
    let mesh_nodes: Vec<crate::models::AgentNode> =
        crate::read_outcome::ReadOutcome::ok_or_unavailable(db::list_agent_nodes_by_mesh(mesh_id))
            .into_result()?;
    let tracked_ids: std::collections::HashSet<String> = mesh_nodes
        .iter()
        .filter(|n| n.status != crate::models::SessionStatus::Archived)
        .filter_map(|n| n.cli_session_id.clone())
        .collect();

    // The app default is mesh-independent, so it applies even when the row
    // is gone (mesh deleted mid-scan) — only the per-Mesh override is lost.
    let app_dir = crate::preferences::worktree_directory();
    let effective: String = db::get_mesh_by_id(mesh_id)
        .ok()
        .map(|m| {
            crate::env::effective_worktree_dir_raw(
                mesh_path,
                m.worktree_directory.as_deref(),
                app_dir.as_deref(),
            )
        })
        .unwrap_or_else(|| {
            crate::env::effective_worktree_dir_raw(mesh_path, None, app_dir.as_deref())
        });

    let mut sessions: Vec<ArchivedAgentNode> = Vec::new();

    sessions.extend(
        discover_claude_sessions_in(&projects_dir, mesh_path, &effective, &tracked_ids)
            .map_err(|e| crate::read_outcome::Unavailable::Read(e.to_string()))?,
    );

    // Cursor keeps the same user/assistant JSONL shape but nests each session
    // below a workspace slug and an id directory. Its project slug is based on
    // the actual workspace path, so include the mesh root and the effective
    // worktree container's `<slug>/<name>` workspaces (issue #1519).
    sessions.extend(discover_cursor_sessions_in(
        &env::cursor_dir().join("projects"),
        mesh_path,
        &tracked_ids,
        Some(&effective),
    ));

    // Command Code stores sessions per-project under
    // `<home>/.commandcode/projects/<encoded-cwd>/<uuid>.jsonl` (issue #1500).
    // Walk the mesh root dir plus its effective worktree container's
    // `<slug>/<name>` dirs so mesh-root and worktree sessions are both found;
    // the session header cwd determines the worktree. The match is anchored
    // (exact root or `{eff-slug}-...`) so sibling repos sharing a slug prefix
    // (e.g. `api` vs `api-gateway`) are never scanned.
    let env_type = EnvType::from(env::env_for_path(Path::new(mesh_path)));
    // `to_spawn_path` is a no-op for already-spawn-form paths on a Windows
    // host; the UNC mapping handles a host-form WSL mesh path
    // (`\\wsl$\<distro>\home\user\repo`), which the CLI slugged as
    // `/home/user/repo`. Drive-letter paths must NOT go through spawn
    // translation for slugging: the Windows CLI slugged `C:\...` directly.
    let spawn_mesh = env::to_spawn_path(Path::new(mesh_path))
        .to_string_lossy()
        .to_string();
    let normalized_mesh = env::normalize_unc_to_wsl(&spawn_mesh);
    // The CLI slugged its own cwd form, so slug the effective container
    // through the same normalization (issue #1519).
    let effective_spawn = env::to_spawn_path(Path::new(&effective))
        .to_string_lossy()
        .to_string();
    let normalized_effective = env::normalize_unc_to_wsl(&effective_spawn);
    if let Some(projects_dir) = env::commandcode_projects_dir(env_type, &normalized_mesh) {
        if projects_dir.is_dir() {
            let encoded_prefix = commandcode_project_slug(&normalized_mesh);
            let effective_slug = commandcode_project_slug(&normalized_effective);
            if let Ok(entries) = fs::read_dir(&projects_dir) {
                for entry in entries.flatten() {
                    let dir_name = entry.file_name().to_string_lossy().to_string();
                    if !is_commandcode_project_dir_for_mesh(
                        &dir_name,
                        &encoded_prefix,
                        &effective_slug,
                    ) {
                        continue;
                    }
                    let dir_path = entry.path();
                    if !dir_path.is_dir() {
                        continue;
                    }
                    sessions.extend(discover_commandcode_sessions_in(
                        Some(dir_path.as_path()),
                        mesh_path,
                        &tracked_ids,
                        Some(&effective),
                    ));
                }
            }
        }
    }

    // Antigravity (`agy`) stores every conversation globally under
    // `~/.gemini/antigravity-cli/brain/<conversation_id>/` (issue #1284).
    // The scanner walks every conversation directory, reads the
    // `.system_generated/logs/transcript.jsonl` for the first USER_INPUT step,
    // and pulls the conversation_id off the directory name (matches the
    // `conversation id: <UUID>` banner the agy TUI prints, so the captured
    // id in `agent_nodes.cli_session_id` aligns 1:1 with this dir name).
    sessions.extend(discover_agy_sessions_in(
        &env::agy_brain_dir(),
        mesh_path,
        &tracked_ids,
        Some(&effective),
    ));

    // Durable archived rows the disk scan found nothing for. Sorting below
    if let Some(path) =
        crate::services::transcript_reader::readers::opencode::opencode_db_path(env_type)
    {
        sessions.extend(
            crate::services::transcript_reader::readers::opencode2::discover(
                &path,
                &normalized_mesh,
                &normalized_effective,
                &opencode2_excluded_sessions(&mesh_nodes, &tracked_ids),
            )
            .map_err(crate::read_outcome::Unavailable::Read)?,
        );
    }

    // Durable archived rows the disk scan found nothing for. Sorting below
    // interleaves them with the discovered sessions by timestamp.
    //
    // Issue #2332 — the same pass attaches each archived row's durable
    // reference to the disk entry that supersedes it, so a merged entry keeps
    // the identity it already had in the DB instead of being flattened down to
    // a bare transcript.
    let durable = durable_archived_entries(&mut sessions, &mesh_nodes);
    sessions.extend(durable);

    // Sort by timestamp descending (most recent first)
    sessions.sort_by(|a, b| {
        let ta = a.timestamp.as_deref().unwrap_or("");
        let tb = b.timestamp.as_deref().unwrap_or("");
        tb.cmp(ta)
    });

    Ok(sessions)
}

/// A V2 migration can retain a copy of a V1 conversation with the same id.
/// Existing node provenance wins until an explicit major-version migration.
fn opencode2_excluded_sessions(
    nodes: &[crate::models::AgentNode],
    tracked: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let mut excluded = tracked.clone();
    excluded.extend(
        nodes
            .iter()
            .filter(|node| {
                crate::agent::provider::resolve_node_provider(node) == Provider::OpenCode
            })
            .filter_map(|node| node.cli_session_id.clone()),
    );
    excluded
}

/// Archived rows the Archive list must keep showing even though no transcript
/// is discoverable for them (issue #1065).
///
/// The pre-#1065 Archive list was built purely from disk, so an archived node
/// vanished from it as soon as it lost its transcript — or immediately, when it
/// never had a `cli_session_id` (an archived Terminal node). The row, its
/// worktree and its branch are all still there, so the archive view lists them
/// with `resumable: false` and the surfaces say Resume is unavailable.
///
/// An archived node whose transcript *is* on disk already appears in
/// `discovered` (archived ids are excluded from `tracked_ids`), so it is not
/// duplicated or downgraded here — the discovered entry is the resumable one.
/// It is not dropped, though: `discovered` is mutated so the surviving entry
/// carries `node_id` (issue #2332). Without that the row's own identity was
/// lost the moment a transcript reappeared, and the list could no longer tell
/// reopening known work from importing external work.
fn durable_archived_entries(
    discovered: &mut [ArchivedAgentNode],
    mesh_nodes: &[crate::models::AgentNode],
) -> Vec<ArchivedAgentNode> {
    let on_disk: std::collections::HashSet<&str> = discovered
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let durable: Vec<ArchivedAgentNode> = mesh_nodes
        .iter()
        .filter(|node| node.status == crate::models::SessionStatus::Archived)
        .filter_map(|node| {
            let session_id = match node
                .cli_session_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                // The disk scan already lists this session, resumably.
                Some(id) if on_disk.contains(id) => return None,
                Some(id) => id.to_string(),
                // No CLI session id to key on (a Terminal node, say). The row
                // id is the only durable identity the archive view has, and
                // both surfaces use this value as the React key / test id.
                None => format!("node-{}", node.id),
            };
            if !seen.insert(session_id.clone()) {
                return None;
            }
            Some(ArchivedAgentNode {
                session_id,
                // The node's name is the only label a row with no transcript
                // can offer.
                first_message: node.name.clone(),
                branch: Some(node.branch.clone()).filter(|branch| !branch.is_empty()),
                cwd: Some(node.path.clone()).filter(|path| !path.is_empty()),
                timestamp: Some(node.created_at.to_rfc3339()),
                worktree_name: node.worktree_name.clone(),
                resumable: false,
                // The row's own recorded executor (issue #2332). Kept verbatim
                // — including a non-resumable one like `terminal`, which is
                // honest here: it explains *why* the entry cannot be resumed,
                // and `resumable: false` is what stops a surface offering it.
                provider: Some(node.provider.clone())
                    .filter(|provider| !provider.trim().is_empty()),
                node_id: Some(node.id),
            })
        })
        .collect();

    // An archived row whose transcript is on disk was skipped above so it is
    // not listed twice — but its identity still belongs on the surviving disk
    // entry (issue #2332). `node_id` is the row's own record, so attaching it
    // is always safe; the scanner's `provider` stays authoritative for a
    // transcript it just read, because the row's value can be a stale executor
    // from an older session.
    for node in mesh_nodes
        .iter()
        .filter(|node| node.status == crate::models::SessionStatus::Archived)
    {
        let Some(session_id) = node
            .cli_session_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        if let Some(entry) = discovered
            .iter_mut()
            .find(|entry| entry.session_id == session_id)
        {
            entry.node_id = Some(node.id);
        }
    }

    durable
}

/// Extract the Buildmesh worktree name from a Cursor project slug (issue
/// #1519). `effective_slug` is `cursor_workspace_slug` of the mesh's
/// effective worktree container: `<eff>-<name>` maps to the node name.
/// `base_slug` preserves the built-in markers after a directory
/// change, so an unrelated
/// subdir slug never becomes a phantom worktree.
fn extract_cursor_worktree_name(
    dir_name: &str,
    base_slug: &str,
    effective_slug: &str,
) -> Option<String> {
    extract_worktree_name(dir_name, base_slug, effective_slug)
}

/// Discover Claude Code sessions below `<claude_dir>/projects` — one JSONL
/// transcript per session id, grouped by encoded project directory.
///
/// Takes the projects root explicitly, like the Cursor, Command Code and AGY
/// scanners, so all four harnesses are testable against a fixture directory
/// and none of them needs the developer's real `~/.claude`.
fn discover_claude_sessions_in(
    projects_dir: &std::path::Path,
    mesh_path: &str,
    effective_dir_raw: &str,
    tracked_ids: &std::collections::HashSet<String>,
) -> Result<Vec<ArchivedAgentNode>, String> {
    if !projects_dir.exists() {
        return Ok(Vec::new());
    }
    let encoded_prefix = encode_path(mesh_path);
    let encoded_effective = encode_path(effective_dir_raw);
    let effective_prefix = format!("{encoded_effective}-");
    let entries = fs::read_dir(projects_dir).map_err(|e| e.to_string())?;
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if !dir_name.starts_with(&encoded_prefix) && !dir_name.starts_with(&effective_prefix) {
            continue;
        }

        let worktree_name = extract_worktree_name(&dir_name, &encoded_prefix, &encoded_effective);

        let dir_path = entry.path();
        if !dir_path.is_dir() {
            continue;
        }

        let jsonl_files = fs::read_dir(&dir_path)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "jsonl")
                    .unwrap_or(false)
            });

        for jsonl_entry in jsonl_files {
            let jsonl_path = jsonl_entry.path();
            let session_id = jsonl_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            if session_id.is_empty() || tracked_ids.contains(&session_id) {
                continue;
            }

            if let Some((first_message, branch, cwd, timestamp)) = parse_session_file(&jsonl_path) {
                if first_message.is_empty() {
                    continue;
                }

                // Use file mtime as fallback timestamp
                let ts = timestamp.or_else(|| {
                    jsonl_entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(|t| {
                            let dt: chrono::DateTime<chrono::Utc> = t.into();
                            dt.to_rfc3339()
                        })
                });

                sessions.push(ArchivedAgentNode {
                    session_id,
                    first_message,
                    branch,
                    cwd,
                    timestamp: ts,
                    worktree_name: worktree_name.clone(),
                    resumable: true,
                    // This scan read `~/.claude/projects`, so Claude Code owns
                    // the conversation — the scanner states its own harness
                    // rather than letting a consumer infer it from the session
                    // id (issue #2332).
                    provider: Some(Provider::Anthropic.to_string()),
                    node_id: None,
                });
            }
        }
    }
    Ok(sessions)
}

/// Discover Cursor's primary workspace transcripts below an explicit projects
/// root. Subagent JSONL is intentionally ignored: it cannot be resumed as the
/// parent Cursor session and lives under the same session directory.
///
/// `effective_dir_raw` is the mesh's effective worktree container
/// (issue #1519); `None` still matches built-in layouts (unit tests
/// without a mesh row).
fn discover_cursor_sessions_in(
    projects_dir: &std::path::Path,
    mesh_path: &str,
    tracked_ids: &std::collections::HashSet<String>,
    effective_dir_raw: Option<&str>,
) -> Vec<ArchivedAgentNode> {
    if !projects_dir.is_dir() {
        return Vec::new();
    }

    let base_slug = cursor_workspace_slug(mesh_path);
    let effective_slug = effective_dir_raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(cursor_workspace_slug)
        .unwrap_or_default();
    let worktree_prefix = if effective_slug.is_empty() {
        format!("{base_slug}--claude-worktrees-")
    } else {
        format!("{effective_slug}-")
    };
    let legacy_prefix = format!("{base_slug}--claude-worktrees-");
    let default_prefix = format!("{base_slug}--worktrees-");
    let mut sessions = Vec::new();
    let Ok(projects) = fs::read_dir(projects_dir) else {
        return sessions;
    };

    for project_entry in projects.flatten() {
        let project_dir = project_entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let dir_name = project_entry.file_name().to_string_lossy().to_string();
        let worktree_name = if dir_name == base_slug {
            None
        } else if dir_name.starts_with(&worktree_prefix)
            || (!legacy_prefix.is_empty() && dir_name.starts_with(&legacy_prefix))
            || dir_name.starts_with(&default_prefix)
        {
            extract_cursor_worktree_name(&dir_name, &base_slug, &effective_slug)
        } else {
            continue;
        };

        let transcripts_dir = project_dir.join("agent-transcripts");
        let Ok(session_dirs) = fs::read_dir(transcripts_dir) else {
            continue;
        };
        for session_entry in session_dirs.flatten() {
            let session_dir = session_entry.path();
            if !session_dir.is_dir() {
                continue;
            }
            let session_id = session_entry.file_name().to_string_lossy().to_string();
            if session_id.is_empty() || tracked_ids.contains(&session_id) {
                continue;
            }

            let jsonl_path = session_dir.join(format!("{session_id}.jsonl"));
            if !jsonl_path.is_file() {
                continue;
            }
            let Some((first_message, branch, cwd, timestamp)) = parse_session_file(&jsonl_path)
            else {
                continue;
            };
            if first_message.is_empty() {
                continue;
            }

            let timestamp = timestamp.or_else(|| {
                jsonl_path
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(|modified| {
                        let dt: chrono::DateTime<chrono::Utc> = modified.into();
                        dt.to_rfc3339()
                    })
            });
            sessions.push(ArchivedAgentNode {
                session_id,
                first_message,
                branch,
                cwd,
                timestamp,
                worktree_name: worktree_name.clone(),
                resumable: true,
                provider: Some(Provider::Cursor.to_string()),
                node_id: None,
            });
        }
    }
    sessions
}

/// Discover Command Code's session files below an explicit project sessions
/// directory (`<home>/.commandcode/projects/<encoded-cwd>/`). The session
/// header supplies the working directory and timestamp; the transcript
/// supplies the first user message for the archive label. Sidecars
/// (`<id>.checkpoints.jsonl`, …) are skipped via `read_session_file`.
fn discover_commandcode_sessions_in(
    sessions_dir: Option<&Path>,
    mesh_path: &str,
    tracked_ids: &std::collections::HashSet<String>,
    effective_dir_raw: Option<&str>,
) -> Vec<ArchivedAgentNode> {
    let Some(sessions_dir) = sessions_dir.filter(|path| path.is_dir()) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }

        let Some(candidate) = commandcode_session::read_session_file(&path) else {
            continue;
        };
        if tracked_ids.contains(&candidate.id) {
            continue;
        }

        // `mesh_path` may be a Windows host UNC path for a WSL mesh while the
        // CLI recorded its in-WSL cwd; match against both forms. The
        // effective container gets the same dual-form treatment so custom
        // absolute WSL locations match (issue #1519).
        let normalized_mesh = env::normalize_unc_to_wsl(mesh_path);
        let effective = effective_dir_raw.map(str::trim).filter(|s| !s.is_empty());
        let normalized_effective = effective.map(|e| env::normalize_unc_to_wsl(e).into_owned());
        let is_mesh_root = env::directories_match(&candidate.directory, mesh_path)
            || env::directories_match(&candidate.directory, &normalized_mesh);
        let worktree_name = if is_mesh_root {
            None
        } else {
            extract_worktree_name_from_path(&candidate.directory, mesh_path)
                .or_else(|| extract_worktree_name_from_path(&candidate.directory, &normalized_mesh))
                .or_else(|| {
                    effective.and_then(|e| worktree_name_under_dir(&candidate.directory, e))
                })
                .or_else(|| {
                    normalized_effective
                        .as_deref()
                        .and_then(|e| worktree_name_under_dir(&candidate.directory, e))
                })
        };
        if !is_mesh_root && worktree_name.is_none() {
            continue;
        }

        let Some(first_message) = parse_commandcode_first_user_message(&path) else {
            continue;
        };
        let timestamp = (candidate.timestamp_ms > 0)
            .then(|| chrono::DateTime::from_timestamp_millis(candidate.timestamp_ms))
            .flatten()
            .map(|timestamp| timestamp.to_rfc3339())
            .or_else(|| {
                path.metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(|modified| {
                        let timestamp: chrono::DateTime<chrono::Utc> = modified.into();
                        timestamp.to_rfc3339()
                    })
            });

        sessions.push(ArchivedAgentNode {
            session_id: candidate.id,
            first_message,
            branch: None,
            cwd: (!candidate.directory.is_empty()).then_some(candidate.directory),
            timestamp,
            worktree_name,
            resumable: true,
            provider: Some(Provider::CommandCode.to_string()),
            node_id: None,
        });
    }
    sessions
}

/// Read the first genuine user prompt from a Command Code transcript for the
/// archive label. Internal blocks and tool-result envelopes have no title text.
fn parse_commandcode_first_user_message(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|kind| kind.as_str()) != Some("message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        if message.get("role").and_then(|role| role.as_str()) != Some("user") {
            continue;
        }
        let text = first_text_block(message.get("content"));
        if is_synthetic_message(&text) {
            continue;
        }
        let display = truncate(&strip_tags(&text), 80);
        if !display.is_empty() {
            return Some(display);
        }
    }
    None
}

/// Discover Antigravity (`agy`) conversations stored under `<brain_dir>/`.
/// AGY writes every conversation globally — not project-scoped like Claude
/// Code — so the scanner walks each `<conversation_id>/` directory and pulls
/// the first `USER_INPUT` step from `.system_generated/logs/transcript.jsonl`.
/// Issue #1284. The `session_id` we return is the directory name, which is
/// the same UUID the agy TUI prints as `conversation id: <UUID>` and the
/// `AgentProvider::resume_args` path feeds back to `--conversation` on resume.
fn discover_agy_sessions_in(
    brain_dir: &std::path::Path,
    mesh_path: &str,
    tracked_ids: &std::collections::HashSet<String>,
    effective_dir_raw: Option<&str>,
) -> Vec<ArchivedAgentNode> {
    if !brain_dir.is_dir() {
        return Vec::new();
    }

    let mut sessions = Vec::new();
    let Ok(entries) = fs::read_dir(brain_dir) else {
        return sessions;
    };

    for entry in entries.flatten() {
        let conv_dir = entry.path();
        if !conv_dir.is_dir() {
            continue;
        }
        let session_id = entry.file_name().to_string_lossy().to_string();
        if session_id.is_empty() || tracked_ids.contains(&session_id) {
            continue;
        }

        let jsonl_path = conv_dir
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        if !jsonl_path.is_file() {
            continue;
        }

        let Some((first_message, workspace_path, created_at)) = parse_agy_transcript(&jsonl_path)
        else {
            continue;
        };
        if first_message.is_empty() {
            continue;
        }

        // Fall back to transcript mtime when no step carried a timestamp —
        // the conversation was written at some point, so the file mtime is
        // always a safe "wall-clock at least this old" anchor for the sort.
        let timestamp = created_at.or_else(|| {
            jsonl_path
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.to_rfc3339()
                })
        });

        // AGY conversations live outside the mesh's own `.claude/projects/`
        // tree, so the worktree name comes purely from the transcript's
        // workspace metadata. Only attach a worktree name when the path
        // resolves inside *this* mesh's worktree convention (effective
        // `<worktree_dir>/<name>`, issue #1519, falling back to legacy
        // `.claude/worktrees/<name>` suffix matching `mesh_path`); anything
        // else stays at the mesh root with no worktree_name so the user
        // sees a global session without a misleading association.
        let worktree_name = workspace_path.as_deref().and_then(|p| {
            extract_worktree_name_from_path_with_effective(p, mesh_path, effective_dir_raw)
        });

        sessions.push(ArchivedAgentNode {
            session_id,
            first_message,
            branch: None,
            cwd: workspace_path,
            timestamp,
            worktree_name,
            resumable: true,
            // AGY resumes with `--conversation <id>`, not `--resume`, so an
            // entry that lost this stamp would be handed to a harness whose
            // CLI has no such flag — or to one that silently starts a new
            // conversation (issue #2332).
            provider: Some(Provider::Agy.to_string()),
            node_id: None,
        });
    }
    sessions
}

/// Map an AGY step's `workspace_path` onto a Buildmesh worktree name when the
/// path lives under the mesh's effective worktree container
/// (`<effective_dir>/<name>`, issue #1519) or the legacy
/// built-in `<mesh>/.worktrees/<name>` and `.claude/worktrees` layouts. Mirrors the
/// `extract_worktree_name` / `extract_cursor_worktree_name` rules used by the
/// other scanners — same separator handling. Returns `None` for the mesh root
/// (no worktree) and for paths that don't resolve to a worktree inside the
/// calling mesh (so a foreign conversation doesn't masquerade as ours).
///
/// `effective_dir_raw` is the mesh's current effective container dir from
/// `env::effective_worktree_dir_raw` (Mesh override → app default →
/// `.worktrees`). `None` still matches built-in layouts (caller couldn't resolve
/// settings, e.g. in a unit test without a DB).
fn extract_worktree_name_from_path(workspace_path: &str, mesh_path: &str) -> Option<String> {
    extract_worktree_name_from_path_with_effective(workspace_path, mesh_path, None)
}

fn extract_worktree_name_from_path_with_effective(
    workspace_path: &str,
    mesh_path: &str,
    effective_dir_raw: Option<&str>,
) -> Option<String> {
    // Issue #1519: try the effective container first (custom dirs, including
    // absolute locations outside the mesh root), then the legacy marker.
    if let Some(effective) = effective_dir_raw.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(name) = worktree_name_under_dir(workspace_path, effective) {
            return Some(name);
        }
    }
    let mesh = mesh_path.replace('\\', "/");
    for directory in [".worktrees", ".claude/worktrees"] {
        let container = format!("{}/{directory}", mesh.trim_end_matches('/'));
        if let Some(name) = worktree_name_under_dir(workspace_path, &container) {
            return Some(name);
        }
    }
    None
}

/// Extract the first path component under `container_dir` when
/// `workspace_path` equals `<container>/<name>` or lives inside it.
/// Separator-normalized (`\` → `/`) and case-aware via `same_path` for the
/// prefix (so Windows drive case doesn't break matching); the returned name
/// preserves its original spelling for display.
fn worktree_name_under_dir(workspace_path: &str, container_dir: &str) -> Option<String> {
    let wp = workspace_path.replace('\\', "/");
    let dir = container_dir.replace('\\', "/");
    let dir_trimmed = dir.trim_end_matches('/');
    let wp_trimmed = wp.trim_end_matches('/');
    // Exact container dir itself is not a worktree (no name).
    if same_path(wp_trimmed, dir_trimmed) {
        return None;
    }
    let prefix = format!("{}/", dir_trimmed.trim_end_matches('/'));
    // Case-insensitive prefix check on Windows to match `same_path`
    // semantics; exact on POSIX.
    let (wp_cmp, prefix_cmp) = if cfg!(target_os = "windows") {
        (wp.to_ascii_lowercase(), prefix.to_ascii_lowercase())
    } else {
        (wp.clone(), prefix.clone())
    };
    let rest_start = wp_cmp.find(&prefix_cmp)?;
    // `find` could match a non-prefix occurrence (e.g. container string
    // appearing later in the path); require position 0.
    if rest_start != 0 {
        return None;
    }
    let rest = &wp[prefix.len()..];
    let name = rest.split('/').next().unwrap_or("").trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Cross-platform path equality for the worktree-association check above.
/// We can't `Path::canonicalize` here (the AGY session may reference a path
/// that doesn't exist on *this* machine), so we normalize separators and
/// case-fold the trailing component — enough to catch the obvious drift
/// while staying lenient about trailing slashes and Windows drive case.
fn same_path(a: &str, b: &str) -> bool {
    let normalize = |s: &str| -> String {
        let trimmed = s.trim_end_matches('/').trim_end_matches('\\');
        if cfg!(windows) {
            trimmed.to_lowercase()
        } else {
            trimmed.to_string()
        }
    };
    normalize(a) == normalize(b)
}

/// Parse an AGY conversation transcript and return the first real user prompt
/// along with the workspace path the conversation was opened against and the
/// earliest step timestamp. The AGY transcript format isn't formally
/// documented, so we accept a few field-name variants rather than betting on
/// one: a missing field just degrades to `None` instead of dropping the row.
fn parse_agy_transcript(
    path: &std::path::Path,
) -> Option<(String, Option<String>, Option<String>)> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut first_prompt: Option<String> = None;
    let mut workspace_path: Option<String> = None;
    let mut earliest_ts: Option<String> = None;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // First-step workspace wins (every step should agree, but the first
        // one is the canonical "where the conversation started" anchor).
        if workspace_path.is_none() {
            workspace_path = extract_agy_workspace_path(&val);
        }

        // Earliest-wins timestamp for sort stability — the first step carries
        // the conversation's true start, later steps are noise for the
        // Archive list.
        if earliest_ts.is_none() {
            earliest_ts = val
                .get("created_at")
                .or_else(|| val.get("timestamp"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }

        // First USER_INPUT step: take the opening prompt verbatim (sans tags,
        // truncated to 80 chars to match the Claude/Cursor scanners).
        if first_prompt.is_none() {
            let step_type = val
                .get("step_type")
                .or_else(|| val.get("type"))
                .and_then(|v| v.as_str());
            if step_type == Some("USER_INPUT") {
                let text = extract_agy_user_text(&val);
                let clean = strip_tags(&text);
                let display = truncate(&clean, 80);
                if !display.is_empty() {
                    first_prompt = Some(display);
                }
            }
        }

        // Cheap early-exit once we have all three.
        if first_prompt.is_some() && workspace_path.is_some() && earliest_ts.is_some() {
            break;
        }
    }

    first_prompt.map(|p| (p, workspace_path, earliest_ts))
}

/// Pull the workspace/cwd out of an AGY step, trying the field names the CLI
/// has been observed to use. Tolerates nested shapes (`metadata.workspace_path`)
/// and the leading-`/` Linux form so a Windows-only field name doesn't drop
/// every WSL session.
fn extract_agy_workspace_path(val: &serde_json::Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "workspace_path",
        "working_directory",
        "cwd",
        "workspace",
        "project_path",
    ];
    for key in KEYS {
        if let Some(s) = val.get(*key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    if let Some(meta) = val.get("metadata") {
        for key in KEYS {
            if let Some(s) = meta.get(*key).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Pull the prompt text out of a USER_INPUT step. AGY has used several
/// shapes — a top-level `text` string, a `content` string, a `content` array
/// of `{type: "text", text: ...}` blocks (Claude-style), and a `message`
/// wrapper. Accept any of them rather than guessing wrong.
fn extract_agy_user_text(val: &serde_json::Value) -> String {
    if let Some(s) = val.get("text").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = val.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = val.get("prompt").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(content) = val.get("content").and_then(|v| v.as_array()) {
        return first_text_block(Some(&serde_json::Value::Array(content.clone())));
    }
    if let Some(message) = val.get("message") {
        if let Some(s) = message.get("text").and_then(|v| v.as_str()) {
            return s.to_string();
        }
        if let Some(s) = message.get("content").and_then(|v| v.as_str()) {
            return s.to_string();
        }
        if let Some(content) = message.get("content") {
            return first_text_block(Some(content));
        }
    }
    String::new()
}

#[cfg(test)]
#[path = "agent_node_discovery_tests.rs"]
mod tests;
