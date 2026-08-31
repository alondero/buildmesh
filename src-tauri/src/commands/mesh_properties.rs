//! Mesh property commands — read/write the user-tunable columns on the
//! `meshes` SQLite row.
//!
//! **There is no `mesh.toml` file.** The "properties" / "config" lives on the
//! `meshes` SQLite row (see `db::get_mesh_by_id`), not in any file at the mesh
//! root. The `MeshRow` struct in `models::MeshRow` is a thin DTO over that row.
//!
//! `Worktree.baseRef` is additionally written to `.claude/settings.json`
//! at the mesh root so Claude Code can read it (see
//! [`update_worktree_base_ref`]); that mirror is an output, not a source of
//! truth — the DB column is the source.
//!
//! Pure sync — each command is a single SQLite read/write (with optional
//! `std::fs::*` for the settings.json mirror on `base_ref` updates). Runs on
//! Tauri's IPC worker, NOT the bounded tokio pool. Issue #1380 review point 4.

use crate::db;
use crate::models::{HarnessConfigValue, MeshRow};
use crate::preferences;
use crate::services::warm_pool;
use std::path::PathBuf;
use tauri::AppHandle;

// ---------------------------------------------------------------------------
// Settings.json helpers (for base_ref only)
// ---------------------------------------------------------------------------

fn write_base_ref(mesh_path: &str, base_ref: &str) -> Result<(), String> {
    let settings_path = PathBuf::from(mesh_path).join(".claude/settings.json");

    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)
            .map_err(|e| format!("failed to read settings.json: {}", e))?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    settings["worktree"]["baseRef"] = serde_json::json!(base_ref);

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create .claude directory: {}", e))?;
    }

    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("failed to serialize settings.json: {}", e))?;
    std::fs::write(&settings_path, content)
        .map_err(|e| format!("failed to write settings.json: {}", e))?;
    Ok(())
}

fn remove_base_ref(mesh_path: &str) -> Result<(), String> {
    let settings_path = PathBuf::from(mesh_path).join(".claude/settings.json");

    let content = std::fs::read_to_string(&settings_path)
        .map_err(|e| format!("failed to read settings.json: {}", e))?;
    let mut settings: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse settings.json: {}", e))?;

    if let Some(obj) = settings.get_mut("worktree") {
        if let Some(obj) = obj.as_object_mut() {
            obj.remove("baseRef");
        }
    }

    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("failed to serialize settings.json: {}", e))?;
    std::fs::write(&settings_path, content)
        .map_err(|e| format!("failed to write settings.json: {}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_mesh_properties(mesh_id: i64) -> Result<MeshRow, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    Ok(MeshRow::from(&mesh))
}

#[tauri::command]
pub fn update_mesh_name(mesh_id: i64, name: String) -> Result<(), String> {
    let db = db::write_conn();
    db.execute(
        "UPDATE meshes SET name = ?1 WHERE id = ?2",
        rusqlite::params![name, mesh_id],
    )
    .map_err(|e| format!("failed to update mesh name: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn update_mesh_column(
    mesh_id: i64,
    column: String,
    value: String,
) -> Result<(), String> {
    // Allowlist of user-tunable `meshes` columns this command can write. The
    // `name` column is a dedicated command (`update_mesh_name`), and
    // `base_ref` / `use_worktree` have settings.json or structural side-effects
    // and route through their own commands. A direct column name is the
    // honest wire shape now that the data lives on the `meshes` row (issue
    // #474); the SQL below still validates against the allowlist rather than
    // interpolating an untrusted column name from the wire.
    const ALLOWED_COLUMNS: &[&str] = &[
        "build_command",
        "run_command",
        // Per-context build/run commands (issue #802). Nullable siblings of
        // build_command/run_command; a Root Node prefers these and falls back
        // to the plain columns when they're unset.
        "root_build_command",
        "root_run_command",
        "model",
        "effort",
        "worktree_mode",
        "default_provider",
    ];
    if !ALLOWED_COLUMNS.contains(&column.as_str()) {
        return Err(format!("unknown mesh column: {}", column));
    }

    let db = db::write_conn();
    db.execute(
        &format!("UPDATE meshes SET {} = ?1 WHERE id = ?2", column),
        rusqlite::params![value, mesh_id],
    )
    .map_err(|e| format!("failed to update mesh column: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn update_mesh_use_worktree(mesh_id: i64, use_worktree: bool) -> Result<(), String> {
    let rows = {
        let db = db::write_conn();
        db.execute(
            "UPDATE meshes SET use_worktree = ?1 WHERE id = ?2",
            rusqlite::params![use_worktree as i32, mesh_id],
        )
        .map_err(|e| format!("failed to update use_worktree: {}", e))?
    };
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    // Issue #1152 — turning worktrees OFF on a mesh that has Autopilot
    // enabled makes Autopilot incompatible (it always forces a worktree
    // for the wrap-up PR branch). Persist `autopilot_enabled = 0` through
    // the existing narrow `set_mesh_autopilot_enabled` write so the next
    // poll pass skips the mesh rather than scheduling an incompatible
    // spawn. We do NOT kill running Agent Nodes — the wrap-up sequence
    // only matters for new spawns, and an in-flight node's worktree was
    // created when the previous setting was active (acceptance criteria
    // 11: "Existing Agent Nodes are not killed by compatibility changes").
    // The change is silent on success — the existing Mesh Properties
    // save indicator covers the user feedback path for the worktree
    // toggle; the autopilot disablement is a side effect the next refresh
    // of the Autopilot Probe tab will reflect.
    if !use_worktree {
        let mesh = db::get_mesh_by_id(mesh_id)
            .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
        if mesh.autopilot_enabled {
            crate::db::set_mesh_autopilot_enabled(mesh_id, false)
                .map_err(|e| format!("failed to clear autopilot_enabled: {}", e))?;
            tracing::info!(
                "update_mesh_use_worktree: disabled autopilot on mesh {} (worktrees off)",
                mesh_id
            );
        }
    }
    Ok(())
}

/// Set the per-mesh target for the pre-spawn Worktree Pool
/// (`services::warm_pool`, issue #611). `0` disables the pool for the
/// mesh; `1..=5` is the target the worker fills to on startup + after
/// each claim. Clamped at the IPC boundary so a misbehaving frontend
/// (or a future bulk-import path) can't write a garbage value to the
/// DB column and break the worker's count/target comparisons.
///
/// On a successful save, schedules a background drain-and-fill via the
/// same `std::thread::spawn` pattern as `spawn::post_spawn_maintenance`.
/// The drain runs off the IPC thread because `git worktree remove` is a
/// blocking syscall (1-3s per worktree on Windows with Defender) — a
/// 5→1 shrink would otherwise freeze the UI for 4-12 seconds. The
/// inner `drain_excess_warm_entries` / `prewarm_one` emit
/// `pool-count-changed` for every actual state change, so the badge
/// settles as rows drop / fill without any explicit end-of-pass emit
/// here (the previous unconditional settle emit was the source of the
/// double-emit when `drain_excess_warm_entries` already fired).
///
/// Dedicated command (not the generic `update_mesh_column` allowlist)
/// so the typed integer + the `0..=5` invariant are enforced here —
/// the catch-all is intentionally unvalidated.
#[tauri::command]
pub fn update_mesh_pool_size(
    app: AppHandle,
    mesh_id: i64,
    pool_size: i32,
) -> Result<(), String> {
    if !(0..=5).contains(&pool_size) {
        return Err(format!(
            "invalid pool size {}: must be 0 (off) or 1..=5",
            pool_size
        ));
    }
    {
        let db = db::write_conn();
        let rows = db
            .execute(
                "UPDATE meshes SET pre_spawn_pool_size = ?1 WHERE id = ?2",
                rusqlite::params![pool_size, mesh_id],
            )
            .map_err(|e| format!("failed to update pre_spawn_pool_size: {}", e))?;
        // An UPDATE that matches no rows silently succeeds otherwise —
        // returning `Ok(())` would let the frontend believe the save
        // succeeded when the mesh was deleted (or never existed) between
        // the load and the save. Surfaces the same contract as
        // `set_mesh_sandbox_inner`'s zero-rows guard.
        if rows == 0 {
            return Err(format!(
                "mesh {} not found (no rows updated)",
                mesh_id
            ));
        }
    }

    // Drain-then-fill runs on a dedicated OS thread so the IPC handler
    // returns immediately. Inner `drain_excess_warm_entries` /
    // `prewarm_one` emit `pool-count-changed` for each state change
    // they make, so the badge settles naturally as rows drop / fill —
    // no explicit settle emit needed (would double-fire when drain
    // already emitted).
    std::thread::spawn(move || {
        warm_pool::drain_and_fill_for_mesh(&app, mesh_id);
    });

    Ok(())
}

/// Return the number of `available` warm pool entries for `mesh_id`.
/// Powers the Worktrees Probe's per-mesh pool badge
/// (`usePoolChanged` listener + `WorktreeManagerTab` UI). Thin wrapper
/// over `db::count_available_warm_for_mesh` — the DB layer is the
/// single source of truth for pool state, so the IPC command is just
/// the typed edge.
#[tauri::command]
pub fn get_mesh_pool_count(mesh_id: i64) -> Result<i64, String> {
    db::count_available_warm_for_mesh(mesh_id)
        .map_err(|e| format!("pool count for mesh {} failed: {}", mesh_id, e))
}

/// Toggle whether this mesh's agent nodes run inside an OS process sandbox
/// (Windows AppContainer #498 / macOS Seatbelt #497). Dedicated command (not
/// the generic `update_mesh_column` allowlist) so it takes a typed `bool` and
/// the zero-rows-is-an-error contract is enforced in `db::set_mesh_sandbox`.
#[tauri::command]
pub fn update_mesh_sandbox(mesh_id: i64, sandbox: bool) -> Result<(), String> {
    db::set_mesh_sandbox(mesh_id, sandbox)
        .map_err(|e| format!("failed to update sandbox: {}", e))
}

/// Persist a mesh's Looping Autopilot configuration (wayfinder #990 /
/// ticket #991). The poller (ticket #992) reads `mode` at startup to
/// decide which spawn strategy to use; the dedicated typed command
/// mirrors `update_mesh_autopilot` (issue #481) so validation lives
/// here, not in the DB helper. The zero-rows guard surfaces a "mesh
/// deleted between load & save" race as an error rather than a silent
/// success — same contract as every other `update_mesh_*` command.
///
/// Validation:
/// - `mode` is the typed `AutopilotMode` enum (no free-form strings).
/// - `max_iterations` is `Option<i32>`: `None` = continuous, `Some(n)`
///   must have `n >= 1` (a cap of 0 would stop the loop on the very
///   first iteration — almost certainly a user mistake).
/// - `interval_seconds` and `consecutive_failures` must be `>= 0` —
///   negative values are nonsensical. No upper bound: the spec defines
///   the defaults (`0`) but no max, and any value the user picks is a
///   deliberate config choice.
#[tauri::command]
pub fn update_mesh_loop_config(
    mesh_id: i64,
    mode: crate::models::AutopilotMode,
    initial_prompt: Option<String>,
    suffix_prompt: Option<String>,
    max_iterations: Option<i32>,
    interval_seconds: i32,
    consecutive_failures: i32,
) -> Result<(), String> {
    // Trim + collapse empty / whitespace-only prompts to NULL — same
    // `clean` shape as `update_mesh_autopilot`, so a blank textfield
    // writes `None` rather than the empty string and the poller's
    // `Some/None` branching on the read side mirrors what the user
    // typed at the IPC edge.
    let clean = |v: Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let initial_prompt = clean(initial_prompt);
    let suffix_prompt = clean(suffix_prompt);

    if let Some(n) = max_iterations {
        if n < 1 {
            return Err(format!(
                "invalid loop_max_iterations {}: must be None (continuous) or >= 1",
                n
            ));
        }
    }
    if interval_seconds < 0 {
        return Err(format!(
            "invalid loop_interval_seconds {}: must be >= 0",
            interval_seconds
        ));
    }
    if consecutive_failures < 0 {
        return Err(format!(
            "invalid loop_consecutive_failures {}: must be >= 0",
            consecutive_failures
        ));
    }

    let rows = db::set_mesh_loop_config(
        mesh_id,
        mode,
        initial_prompt.as_deref(),
        suffix_prompt.as_deref(),
        max_iterations,
        interval_seconds,
        consecutive_failures,
    )
    .map_err(|e| format!("failed to update loop config: {}", e))?;
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    Ok(())
}

/// Persist a mesh's Autopilot Policy (issue #481, PRD #480) in one write.
/// Dedicated typed command (not the string-only `update_mesh_column`
/// catch-all) so the enabled flag stays a real `bool`, the concurrency
/// limit is range-checked here, and the five columns land atomically —
/// the poller (`services::autopilot`) reads them as one policy.
///
/// Validation and normalisation, mirroring `validate_pr_spawn_inputs`'
/// trim-then-collapse contract:
/// - `concurrency_limit` must be `1..=8` (PRD example is 2; 8 is a sane
///   local-machine ceiling).
/// - `trigger_label` / `provider` / `action_on_success` are trimmed;
///   empty-after-trim collapses to NULL so the model reads back `None`
///   and the poller applies its defaults (`buildmesh:run`, the normal
///   provider chain, `draft_pr`).
#[tauri::command]
pub fn update_mesh_autopilot(
    mesh_id: i64,
    enabled: bool,
    trigger_label: Option<String>,
    concurrency_limit: i32,
    provider: Option<String>,
    action_on_success: Option<String>,
) -> Result<(), String> {
    if !(1..=8).contains(&concurrency_limit) {
        return Err(format!(
            "invalid autopilot concurrency limit {}: must be 1..=8",
            concurrency_limit
        ));
    }
    let clean = |v: Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let trigger_label = clean(trigger_label);
    let provider = clean(provider);
    let action_on_success = clean(action_on_success);
    if let Some(action) = action_on_success.as_deref() {
        if !["draft_pr", "pr", "none"].contains(&action) {
            return Err(format!("unknown autopilot action_on_success: {}", action));
        }
    }
    // Issue #1152 — enforce compatibility on the write side. Read the
    // mesh row to evaluate the verdict with the *new* `provider`
    // selection in place; reject the write with the structured reason
    // text if the new state would be incompatible and the caller is
    // trying to enable. Disabling (`enabled = false`) is always allowed
    // — the user is opting out, never opting in to an invalid run.
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    if enabled {
        let app_default_provider = preferences::default_provider();
        let verdict = crate::autopilot::compatibility::compute_for_mesh(
            provider.as_deref().or(mesh.autopilot_provider.as_deref()),
            mesh.default_provider.as_deref(),
            app_default_provider.as_deref(),
            mesh.use_worktree,
        );
        if !verdict.allowed {
            let reasons = format_reasons(&verdict.reasons);
            return Err(format!(
                "cannot enable Autopilot on mesh {}: incompatible Spawn Option {} ({reasons}). \
                 Pick a compatible harness, or correct the mesh configuration.",
                mesh_id, verdict.resolved_spawn_option.as_deref().unwrap_or("<none>"),
            ));
        }
    }
    let rows = db::set_mesh_autopilot(
        mesh_id,
        enabled,
        trigger_label.as_deref(),
        concurrency_limit,
        provider.as_deref(),
        action_on_success.as_deref(),
    )
    .map_err(|e| format!("failed to update autopilot policy: {}", e))?;
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    Ok(())
}

/// Format a list of [`AutopilotCompatibilityReason`]s into a user-facing
/// string suitable for a Tauri command's `Err` message. The Autopilot Probe
/// UI also surfaces these reasons verbatim — the formatter is the canonical
/// place to translate them into prose so both surfaces stay aligned.
fn format_reasons(reasons: &[crate::autopilot::compatibility::AutopilotCompatibilityReason]) -> String {
    use crate::autopilot::compatibility::AutopilotCompatibilityReason;
    let mut parts: Vec<String> = Vec::with_capacity(reasons.len());
    for reason in reasons {
        let part = match reason {
            AutopilotCompatibilityReason::NoResolvedHarness => {
                "no Agent Harness resolved".to_string()
            }
            AutopilotCompatibilityReason::UnknownHarness { harness_id } => {
                format!("Agent Harness '{}' is not installed", harness_id)
            }
            AutopilotCompatibilityReason::PlainTerminal => {
                "Terminal is a plain shell, not an Agent Harness".to_string()
            }
            AutopilotCompatibilityReason::MissingPrefill { harness_id } => {
                format!(
                    "Agent Harness '{}' does not accept a startup prompt",
                    harness_id
                )
            }
            AutopilotCompatibilityReason::MissingAttentionHook { harness_id } => {
                format!(
                    "Agent Harness '{}' does not install an attention hook",
                    harness_id
                )
            }
            AutopilotCompatibilityReason::WorktreeDisabled => {
                "worktrees are disabled on this mesh".to_string()
            }
        };
        parts.push(part);
    }
    parts.join("; ")
}

/// Toggle a mesh's Looping Autopilot on/off — the Start/Stop control on the
/// Autopilot Probe tab (ticket #994). Looping mode is DB-config-driven: the
/// poller (`services::autopilot`) spawns iterations for any mesh where
/// `autopilot_enabled = 1` AND `autopilot_mode = Looping` AND a non-empty
/// `loop_initial_prompt` is set. This command flips ONLY `autopilot_enabled`
/// via the narrow `db::set_mesh_autopilot_enabled` write, so Start/Stop can't
/// clobber the issue-driven policy columns (`update_mesh_autopilot` owns those).
/// Takes effect on the next poll pass (≤ `services::autopilot::POLL_INTERVAL`)
/// — no restart needed, since the poller re-reads the enabled-mesh set every
/// pass. Zero-rows guard surfaces a "mesh deleted between load & save" race as
/// an error rather than a silent success.
#[tauri::command]
pub fn set_mesh_autopilot_enabled(mesh_id: i64, enabled: bool) -> Result<(), String> {
    // Issue #1152 — read-side check before the narrow write. Reject the
    // enable when the resolved Spawn Option is incompatible (same
    // verdict the Probe UI displays). Disabling is always allowed.
    if enabled {
        let mesh = db::get_mesh_by_id(mesh_id)
            .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
        let app_default_provider = preferences::default_provider();
        let verdict = crate::autopilot::compatibility::compute_for_mesh(
            mesh.autopilot_provider.as_deref(),
            mesh.default_provider.as_deref(),
            app_default_provider.as_deref(),
            mesh.use_worktree,
        );
        if !verdict.allowed {
            let reasons = format_reasons(&verdict.reasons);
            return Err(format!(
                "cannot enable Autopilot on mesh {}: incompatible Spawn Option {} ({reasons}). \
                 Pick a compatible harness, or correct the mesh configuration.",
                mesh_id, verdict.resolved_spawn_option.as_deref().unwrap_or("<none>"),
            ));
        }
    }
    let rows = db::set_mesh_autopilot_enabled(mesh_id, enabled)
        .map_err(|e| format!("failed to update autopilot_enabled: {}", e))?;
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    Ok(())
}

/// Runtime status of a mesh's Looping Autopilot for the Probe tab's status
/// badge (ticket #994). Reads the mesh's `autopilot_enabled` flag +
/// the loop-iteration ledger (`db::list_loop_iterations`) and returns the
/// pure-derived [`crate::services::autopilot::LoopStatusDto`] (Active N /
/// Idle / Stopped). No GitHub, no scheduler state — the loop is DB-driven, so
/// status is a projection of the ledger + the enabled flag.
#[tauri::command]
pub fn get_loop_status(
    mesh_id: i64,
) -> Result<crate::services::autopilot::LoopStatusDto, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let rows = db::list_loop_iterations(mesh_id)
        .map_err(|e| format!("failed to list loop iterations for mesh {}: {}", mesh_id, e))?;
    Ok(crate::services::autopilot::derive_loop_status(
        mesh.autopilot_enabled,
        &rows,
    ))
}

/// Pure compatibility verdict for Autopilot on one Mesh (issue #1152).
///
/// Composes the three layers the UI needs:
/// 1. Resolves the Spawn Option Autopilot will launch (`meshes.autopilot_provider`
///    explicit → mesh default → app default → `"claude"`).
/// 2. Looks up the harness's authoritative capability descriptor.
/// 3. Runs the pure evaluator and returns the verdict + reason codes.
///
/// The command is read-only — no DB writes. The Mesh is fetched via
/// `db::get_mesh_by_id` so an unknown mesh id surfaces as an error rather
/// than a silent "incompatible" verdict (the front-end then shows "Mesh
/// not found"). `use_worktree` is read straight off the row; the App-wide
/// default provider is read via `preferences::default_provider()` to keep
/// the layer-3 lookup in lock-step with `resolve_default_provider`'s
/// existing fall-through (`"claude"` hardcoded fallback if every layer
/// was empty).
#[tauri::command]
pub fn get_autopilot_compatibility(
    mesh_id: i64,
) -> Result<crate::autopilot::compatibility::AutopilotCompatibility, String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;
    let app_default_provider = preferences::default_provider();
    Ok(crate::autopilot::compatibility::compute_for_mesh(
        mesh.autopilot_provider.as_deref(),
        mesh.default_provider.as_deref(),
        app_default_provider.as_deref(),
        mesh.use_worktree,
    ))
}

#[tauri::command]
pub fn update_worktree_base_ref(mesh_id: i64, base_ref: String) -> Result<(), String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Map 'fresh' → origin/main and 'head' → HEAD
    let resolved = match base_ref.as_str() {
        "fresh" => "origin/main".to_string(),
        "head" => "HEAD".to_string(),
        other => other.to_string(),
    };

    // Write to both DB and settings.json
    {
        let db = db::write_conn();
        db.execute(
            "UPDATE meshes SET base_ref = ?1 WHERE id = ?2",
            rusqlite::params![resolved, mesh_id],
        )
        .map_err(|e| format!("failed to update base_ref in DB: {}", e))?;
    }

    write_base_ref(&mesh.path, &resolved)
}

#[tauri::command]
pub fn remove_worktree_base_ref(mesh_id: i64) -> Result<(), String> {
    let mesh = db::get_mesh_by_id(mesh_id)
        .map_err(|e| format!("mesh {} not found: {}", mesh_id, e))?;

    // Write default to DB and remove from settings.json
    {
        let db = db::write_conn();
        db.execute(
            "UPDATE meshes SET base_ref = 'origin/main' WHERE id = ?1",
            rusqlite::params![mesh_id],
        )
        .map_err(|e| format!("failed to reset base_ref in DB: {}", e))?;
    }

    remove_base_ref(&mesh.path)
}

// ---------------------------------------------------------------------------
// Per-Mesh harness overrides (issue #1151 / slice 2 of #1148)
// ---------------------------------------------------------------------------
//
// Each Mesh owns a sparse `HashMap<String, HarnessConfigValue>` written to
// the `meshes.harness_overrides` JSON column at schema v33. The cascade
// at the spawn seam is now: explicit > mesh override > application
// default > native. The user-facing surface (separate per-harness
// override rows + a Reset-all bulk action) lives in
// `MeshPropertiesTab` — these commands are the typed edge behind it.
//
// Validation lives here at the IPC boundary (issue #1148 acceptance
// criteria 5: "Unknown harness ids and invalid effort values are
// rejected at the backend boundary"). The DB helpers are the typed
// write pass; the validator (`preferences::validate_harness_default`)
// is re-used verbatim so the harness-id and effort-vocabulary rules
// can't drift between the application-default and per-mesh-override
// paths.

/// Upsert one harness's entry in the mesh's `harness_overrides` map
/// (issue #1151 / slice 2 of #1148). Equivalent to the App Settings
/// `set_harness_default` command but scoped to a single Mesh: it
/// validates the harness id and effort vocabulary against the same
/// capability descriptor, then writes the JSON map. An empty
/// post-validation value removes the entry (sparse-map invariant).
/// Validation runs BEFORE the write so a rejected call never produces
/// a half-edited map (the DB helper is also safe to call on a
/// not-found mesh — it returns 0 rows, which the IPC surface maps to
/// an error).
#[tauri::command]
pub fn upsert_mesh_harness_override(
    mesh_id: i64,
    harness_id: String,
    value: HarnessConfigValue,
) -> Result<(), String> {
    // Validate first (issue #1148 AC #5). The validator trims + collapses
    // blanks via `preferences::normalize_harness_default` so a save
    // carrying only whitespace is rejected as an empty map entry (the
    // helper then drops the entry to maintain the sparse invariant).
    let validated = preferences::validate_harness_default(&harness_id, value)?;
    let rows = db::upsert_mesh_harness_override(mesh_id, &harness_id, validated)
        .map_err(|e| format!("failed to upsert mesh harness override: {}", e))?;
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    Ok(())
}

/// Remove one harness's entry from the mesh's `harness_overrides` map
/// (issue #1151 / slice 2 of #1148). Idempotent — clearing a harness
/// that had no override is a no-op. The resolved value at the next
/// spawn falls back to the application default (then native) for that
/// harness. Does NOT validate the harness id — the validator is the
/// typed-write gate, and the IPC caller has already loaded the mesh's
/// override list (so a typo in the harness id is impossible from the
/// UI flow). A bad harness id passed directly surfaces as a silent
/// no-op row "touch" — the row count is the source of truth for the
/// "mesh not found" error.
#[tauri::command]
pub fn remove_mesh_harness_override(
    mesh_id: i64,
    harness_id: String,
) -> Result<(), String> {
    let rows = db::remove_mesh_harness_override(mesh_id, &harness_id)
        .map_err(|e| format!("failed to remove mesh harness override: {}", e))?;
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    Ok(())
}

/// Reset every entry in the mesh's `harness_overrides` map (issue #1151)
/// — the secondary "Reset all" bulk action on the Mesh Properties tab.
/// Distinct from the per-harness reset: this clears the entire map,
/// restoring the Mesh to "inherit every application default" for every
/// Agent Harness. The application-level defaults map is NOT touched —
/// the user can still configure application defaults per harness and
/// the Mesh will inherit them cleanly after the reset.
#[tauri::command]
pub fn clear_mesh_harness_overrides(mesh_id: i64) -> Result<(), String> {
    let rows = db::clear_mesh_harness_overrides(mesh_id)
        .map_err(|e| format!("failed to clear mesh harness overrides: {}", e))?;
    if rows == 0 {
        return Err(format!("mesh {} not found (no rows updated)", mesh_id));
    }
    Ok(())
}
