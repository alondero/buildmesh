//! Mesh and Autopilot-mode wire types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ts_rs::TS;

use super::HarnessConfigValue;

/// Discriminator the v30 autopilot poller reads to decide which spawn
/// strategy to use (wayfinder #990 / ticket #991). Persisted as TEXT on
/// `meshes.autopilot_mode` (default `'issue_driven'` so every pre-v30 mesh
/// keeps the GitHub-label behaviour byte-for-byte). The wire shape is the
/// same `snake_case` union as [`SessionStatus`] — multi-word variants keep
/// their underscore (issue #359 lesson), so `IssueDriven` round-trips as
/// `"issue_driven"` and `Looping` as `"looping"`. `#[default] = IssueDriven`
/// so `Mesh::default()` (issue #518) inherits the pre-v30 behaviour without
/// a fixture edit.
///
/// Lives in `models` (not `db`) because the `Mesh` struct embeds it as a
/// field — `db` already imports `models::*`, so a domain-cycle (models ->
/// db for the enum) would be required if the enum lived in `db`. Domain
/// concept (autopilot mode discriminator) that happens to be stored on the
/// `meshes` row, so model-resident is also semantically correct.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case", export_to = "AutopilotMode.ts")]
pub enum AutopilotMode {
    /// The default, pre-v30 behaviour: the background poller watches the
    /// mesh's GitHub repo for `autopilot_trigger_label`-tagged issues and
    /// spawns branched-worktree Agent Nodes for them as they appear.
    #[default]
    IssueDriven,
    /// The new Looping mode (tickets #992 / #993): one node per loop
    /// iteration, sequential, driven by `loop_initial_prompt` and
    /// optionally suffix-injected with `loop_suffix_prompt` between
    /// iterations. Spawns on the mesh's configured worktree strategy
    /// (`mesh.use_worktree`), NOT the autopilot-forced-branched mode.
    Looping,
}

impl AutopilotMode {
    /// The literal DB string the `meshes.autopilot_mode` column stores.
    /// Pinned here (not derived from serde) so a `serde` rename_all drift
    /// can't silently corrupt the DB column — the same rationale as
    /// [`SessionStatus::to_db_str`].
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::IssueDriven => "issue_driven",
            Self::Looping => "looping",
        }
    }

    /// Parse back from the DB column. Unknown strings degrade to
    /// `IssueDriven` (the pre-v30 default) rather than `None`, so a row an
    /// old build accidentally wrote with an unsupported value doesn't break
    /// the poller (a `None` here would be worse than a degraded default —
    /// the poller would have to special-case `Option` everywhere).
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "looping" => Self::Looping,
            _ => Self::IssueDriven,
        }
    }
}

/// A mesh — top-level folder containing agent nodes.
///
/// Generated to src/types/generated/Mesh.ts (issue #359). `i64` fields carry
/// `#[ts(as = "i32")]` so they emit `number` (serde_json sends JS numbers, not
/// the `bigint` ts-rs defaults to for 64-bit ints).
///
/// `#[derive(Default)]` (issue #518) so test fixtures and stub-only call
/// sites can spread `..Default::default()` instead of re-listing every field
/// on each new column. Follow-up to the `AgentNode` migration in #457.
/// Semantics are Option A (zero-value stub): every scalar is `0`/`""`/
/// `false` and every `Option<T>` is `None`. `created_at` defaults to
/// UNIX epoch (chrono's `DateTime::<Utc>::default()`), which is a
/// well-defined placeholder that won't accidentally match a real row.
/// Future `Option<T>` columns automatically inherit `None` with no
/// fixture edits.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "Mesh.ts")]
pub struct Mesh {
    #[ts(as = "i32")]
    pub id: i64,
    pub name: String,
    pub path: String, // absolute path to mesh root
    pub layout: String, // 'grid' or 'single'
    #[ts(as = "i32")]
    pub position: i64, // sort order in sidebar
    pub created_at: DateTime<Utc>,
    // Mesh-level config (see MeshRow for the canonical typed view)
    pub build_command: Option<String>,
    pub run_command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub use_worktree: bool, // default true
    pub worktree_mode: Option<String>,
    pub default_provider: Option<String>,
    pub base_ref: String, // default "origin/main"
    /// Free-form scratch pad text for the Probe Panel "📝 Scratch Pad"
    /// tab. Owned by Buildmesh only — never written to disk, never visible
    /// to agents. Persisted as `meshes.scratchpad TEXT NOT NULL DEFAULT ''`
    /// (schema v17) and read back as the raw `String`. Empty string is a
    /// normal, non-error state ("no notes yet").
    pub scratchpad: String,
    /// OS-level agent process sandbox toggle. When `true`, agent PTY
    /// processes spawned in this mesh are confined to the node's Git
    /// worktree — macOS Seatbelt (`sandbox-exec`, #497) and Windows
    /// AppContainer (#498) each read this flag and apply their own
    /// confinement policy. Off by default (`false`); ignored on hosts
    /// where neither native spawn is built. Persisted as
    /// `meshes.sandbox INTEGER NOT NULL DEFAULT 0` (schema v18).
    pub sandbox: bool,
    /// Per-mesh target for the pre-spawn Worktree Pool worker
    /// (`services::warm_pool`, issue #609 / v21). `0` disables the pool
    /// for this mesh (no warm entries created on startup, no refill
    /// after claim); `1..=5` is the target the worker fills to.
    /// Clamped at the IPC boundary (`update_mesh_pool_size`), not here
    /// — this field is the typed integer the worker reads. ON by
    /// default since schema v24 (`1`, ADR 0020); opted out via the
    /// Worktrees Probe's ConfigurationCard (issue #611). Persisted as
    /// `meshes.pre_spawn_pool_size INTEGER NOT NULL DEFAULT 1`
    /// (schema v22, default flipped in v24).
    #[ts(as = "i32")]
    pub pre_spawn_pool_size: i32,
    /// User-chosen accent colour for the mesh, as a `#rrggbb` hex string.
    /// Picked in the "New mesh" modal on creation and recolourable by
    /// clicking the mesh's colour swatch in the sidebar. `None` means the
    /// user never chose one, so the frontend falls back to the deterministic
    /// palette keyed on the mesh id (`src/lib/meshColors.ts`). Persisted as
    /// `meshes.color TEXT` (schema v25); empty/absent reads back as `None`.
    pub color: Option<String>,
    /// Autopilot Mode master switch (issue #481, PRD #480). When `true` the
    /// background poller (`services::autopilot`) watches this mesh's GitHub
    /// repo for issues tagged [`Mesh::autopilot_trigger_label`] and spawns
    /// branched-worktree Agent Nodes for them automatically. Persisted as
    /// `meshes.autopilot_enabled INTEGER NOT NULL DEFAULT 0` (schema v26).
    pub autopilot_enabled: bool,
    /// GitHub issue label that marks an issue as an Autopilot task. `None`
    /// falls back to [`DEFAULT_AUTOPILOT_TRIGGER_LABEL`] at poll time.
    pub autopilot_trigger_label: Option<String>,
    /// Maximum number of concurrently *active* auto-spawned nodes for this
    /// mesh. The poller only ingests new issues while the active count is
    /// below this limit (PRD #480 story 5/6). Clamped to `1..=8` at the IPC
    /// boundary; stored as `INTEGER NOT NULL DEFAULT 2`.
    #[ts(as = "i32")]
    pub autopilot_concurrency_limit: i32,
    /// Spawn Option id auto-spawned nodes use. `None` falls through the
    /// normal default-provider chain (mesh default → app default → claude).
    pub autopilot_provider: Option<String>,
    /// What Autopilot asks the agent to do once the wrap-up verification
    /// passes: `"draft_pr"` (default) opens a draft PR, `"pr"` opens a
    /// ready-for-review PR, `"none"` stops after push.
    pub autopilot_action_on_success: Option<String>,
    /// Root-context build command (issue #802). When set, a node running at
    /// the mesh root (`env::worktree_segment(node).is_none()`) runs this
    /// instead of [`build_command`](Self::build_command); Worktree Nodes keep
    /// running `build_command`. `None` falls back to `build_command` in both
    /// contexts — the historical PR #801 behaviour. Persisted as
    /// `meshes.root_build_command TEXT` (schema v27).
    pub root_build_command: Option<String>,
    /// Root-context run command (issue #802) — the run-mode sibling of
    /// [`root_build_command`](Self::root_build_command). `None` falls back to
    /// `run_command`. Persisted as `meshes.root_run_command TEXT` (schema v27).
    pub root_run_command: Option<String>,
    /// Discriminator the v30 autopilot poller reads to decide which spawn
    /// strategy to use (wayfinder #990 / ticket #991). The default
    /// `IssueDriven` matches the pre-v30 GitHub-label poller byte-for-byte;
    /// `Looping` is the new sequential prompt-driven mode implemented by
    /// tickets #992 + #993. Persisted as `meshes.autopilot_mode TEXT NOT
    /// NULL DEFAULT 'issue_driven'` (schema v30).
    pub autopilot_mode: AutopilotMode,
    /// Body of the prompt injected into every loop-iteration node when
    /// `autopilot_mode == Looping` (wayfinder #990). `None` is a "no
    /// prompt configured" state — the poller (ticket #992) treats it as
    /// "loop not ready, stay idle" rather than fabricating a prompt.
    /// Persisted as `meshes.loop_initial_prompt TEXT` (schema v30);
    /// empty/absent reads back as `None`.
    pub loop_initial_prompt: Option<String>,
    /// Optional second-turn prompt injected AFTER the issue-style wrap-up
    /// (#485) verifies green, before the next loop iteration starts
    /// (ticket #993). `None` = no suffix turn — the iteration completes
    /// as soon as wrap-up passes. Persisted as
    /// `meshes.loop_suffix_prompt TEXT` (schema v30); empty/absent reads
    /// back as `None`.
    pub loop_suffix_prompt: Option<String>,
    /// Optional hard cap on loop iterations (wayfinder #990). `None` =
    /// continuous — the user must intervene to stop the loop. `Some(n)`
    /// with `n >= 1` = stop after n iterations. Validated at the IPC
    /// boundary (`commands::mesh_properties::update_mesh_loop_config`)
    /// to `>= 1` when set. Persisted as `meshes.loop_max_iterations
    /// INTEGER` (schema v30); nullable to carry the "no cap" meaning
    /// past the row. `i32` matches the `autopilot_concurrency_limit`
    /// precedent — a sane upper bound for a user-configured cap and
    /// keeps the wire shape `number` (not `bigint`).
    pub loop_max_iterations: Option<i32>,
    /// Pause delay between consecutive loop spawns (wayfinder #990).
    /// The poller (ticket #992) re-checks after this many seconds; `0`
    /// means "spawn as soon as the previous iteration finished" (no
    /// pause). Persisted as `meshes.loop_interval_seconds INTEGER NOT
    /// NULL DEFAULT 0` (schema v30).
    pub loop_interval_seconds: i32,
    /// Consecutive-failure auto-pause threshold (wayfinder #990). When
    /// `>= this value` consecutive loop iterations wrap-up-failed, the
    /// poller stops spawning until the user clears or resets it. `0`
    /// (the default) disables the threshold. Persisted as
    /// `meshes.loop_consecutive_failures INTEGER NOT NULL DEFAULT 0`
    /// (schema v30).
    pub loop_consecutive_failures: i32,
    /// **Per-Mesh harness overrides** (issue #1151 / slice 2 of #1148) —
    /// a sparse map keyed by stable harness profile id (the same id the
    /// Spawn Menu uses, e.g. `"claude"`, `"codex"`, `"agy"`, plus any
    /// user-defined custom profile id). A present entry supplies a
    /// per-harness model and/or effort value that overrides the
    /// application-level default for that harness only on this Mesh;
    /// resolving per field follows the cascade order
    /// (explicit > mesh override > application > native). A missing key
    /// means "this Mesh inherits the application default for that
    /// harness". The map is **sparse**: an entry whose every field
    /// collapses to absent is removed entirely by the CRUD command, so
    /// a stored key is never `{model: null, effort: null}`.
    ///
    /// Persisted as `meshes.harness_overrides TEXT NOT NULL DEFAULT '{}'`
    /// (schema v33), serialised as a JSON object. The legacy
    /// `meshes.model` / `meshes.effort` columns remain physically present
    /// for positional row compatibility but are no longer read as active
    /// configuration; the v33 one-shot migration copies non-empty
    /// legacy values into a `claude` override entry.
    pub harness_overrides: HashMap<String, HarnessConfigValue>,
    /// Per-mesh cap on **concurrent admitted circuit runs** (issue #1467).
    /// One slot per admitted run regardless of how many agent nodes the
    /// run's blueprint fans out to — fixes the two-run overlap
    /// PR-review deadlock where the legacy `autopilot_concurrency_limit`
    /// was incorrectly used as a circuit scheduler limit. Default 2
    /// unlocks the two-overlap acceptance criterion out of the box.
    /// Validated to `1..=8` at the IPC
    /// boundary (`commands::mesh_properties::update_mesh_circuit_run_capacity`),
    /// mirroring the legacy column. Persisted as
    /// `meshes.circuit_run_capacity INTEGER NOT NULL DEFAULT 2` (schema
    /// v36); pre-v36 rows read back as 2 via the `COALESCE(col, 2)` in
    /// `migrations::mesh_columns_projection`. Stored as `i32` to match
    /// the legacy column's wire shape (`number` in TS, not `bigint`).
    #[ts(as = "i32")]
    pub circuit_run_capacity: i32,
    /// Per-Mesh Worktree Node directory override (issue #1519).
    /// Optional raw user input with the same semantics as
    /// [`crate::preferences::AppPreferences::worktree_directory`]:
    /// relative resolves from the Mesh root, absolute must match the
    /// Mesh's host environment, blank collapses to `None` (inherit).
    /// Precedence: Mesh override → application default →
    /// `.claude/worktrees` under the Mesh root. Changing it affects
    /// future nodes only — live nodes keep their persisted
    /// [`AgentNode::worktree_path`]. Persisted as
    /// `meshes.worktree_directory TEXT` (schema v37); empty/absent
    /// reads back as `None`.
    pub worktree_directory: Option<String>,
}

/// Fallback for [`Mesh::autopilot_trigger_label`] when the user enables
/// Autopilot without customizing the label (PRD #480 uses this literal).
pub const DEFAULT_AUTOPILOT_TRIGGER_LABEL: &str = "buildmesh:run";

/// The folder chosen in the "New mesh" modal's location picker. Returned by
/// the `pick_mesh_folder` command so the frontend can show the selected
/// path (and derived name) before committing the create — the native folder
/// dialog is a backend-only capability, so this splits "pick a folder" from
/// "create the mesh" (which used to be fused in `add_mesh`). `None` from the
/// command means the user cancelled the dialog.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "PickedFolder.ts")]
pub struct PickedFolder {
    pub path: String,
    pub name: String,
}
/// App settings stored in SQLite
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub default_projects_root: String,
    pub windows_cli_path: String,
    pub wsl_cli_path: String,
}

/// Typed view of a `Mesh` row — a 1:1 mirror of the user-tunable columns on
/// the `meshes` SQLite row (name, build/run commands, model, effort,
/// base_ref, use_worktree, worktree_mode, default_provider). This is the
/// single typed view of mesh config used by every consumer (frontend
/// properties, agent spawning, build/run). Construct it via
/// `MeshRow::from(&mesh)` — never hand-copy `Mesh` fields elsewhere.
///
/// **There is no `mesh.toml` file.** This struct is a thin DTO over a
/// `meshes` SQLite row (see `db::get_mesh_by_path`); every field on it
/// is a column on that row. The "config" in the previous name is
/// historical — before the DB columns existed, mesh settings lived in a
/// TOML file at the mesh root; that file was deleted when the columns
/// were added (see `docs/adr/` and `docs/specs/build-run-system.md` for
/// the migration history). New contributors reading `MeshRow` should
/// read it as "the DTO that mirrors a `meshes` row" and treat the
/// `meshes` table as the single source of truth. The `base_ref` field
/// is *also* mirrored into `.claude/settings.json` at the mesh root
/// (see `commands::mesh_properties::update_worktree_base_ref`) for
/// Claude Code to read; that mirror is an output, not an input to
/// spawn-time resolution.
///
/// Generated to src/types/generated/MeshRow.ts (issue #404 / issue #474).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "MeshRow.ts")]
pub struct MeshRow {
    pub name: Option<String>,
    pub build_command: Option<String>,
    pub run_command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub base_ref: Option<String>,
    pub use_worktree: bool,
    pub worktree_mode: Option<String>,
    pub default_provider: Option<String>,
    /// OS-level sandbox toggle (macOS Seatbelt #497, Windows AppContainer
    /// #498) — see [`Mesh::sandbox`]. The column is one; the OS-specific
    /// spawn policy is decided at `spawn_environment::wrap` time.
    pub sandbox: bool,
    /// Per-mesh pre-spawn pool target — see [`Mesh::pre_spawn_pool_size`].
    /// `0` = pool off, `1..=5` = target the worker fills to. Surfaced in
    /// the Worktrees Probe's ConfigurationCard (issue #611).
    #[ts(as = "i32")]
    pub pre_spawn_pool_size: i32,
    /// Autopilot Policy (issue #481) — see the matching [`Mesh`] fields.
    pub autopilot_enabled: bool,
    pub autopilot_trigger_label: Option<String>,
    #[ts(as = "i32")]
    pub autopilot_concurrency_limit: i32,
    pub autopilot_provider: Option<String>,
    pub autopilot_action_on_success: Option<String>,
    /// Per-context build/run commands (issue #802). When set, a Root Node
    /// runs these instead of `build_command` / `run_command`; `None` falls
    /// back to those. See the matching [`Mesh`] fields.
    pub root_build_command: Option<String>,
    pub root_run_command: Option<String>,
    /// Looping Autopilot configuration (wayfinder #990 / ticket #991).
    /// See the matching [`Mesh`] fields — every `loop_*` column is
    /// surfaced here so the dedicated Autopilot Probe UI tab (ticket #994)
    /// reads & writes them through the same `get_mesh_properties` IPC
    /// boundary. `loop_consecutive_failures` IS the configured auto-pause
    /// threshold (default `0` = feature off), NOT a runtime failure count —
    /// the running count lives in process state (`#992` follow-up).
    pub autopilot_mode: AutopilotMode,
    pub loop_initial_prompt: Option<String>,
    pub loop_suffix_prompt: Option<String>,
    pub loop_max_iterations: Option<i32>,
    pub loop_interval_seconds: i32,
    pub loop_consecutive_failures: i32,
    /// **Per-Mesh harness overrides** (issue #1151 / slice 2 of #1148) —
    /// see the matching [`Mesh`] field. Surface for the Mesh Properties
    /// "Per-harness overrides" experience; the legacy `model` / `effort`
    /// fields stay here so a pre-v33 reading client doesn't crash, but
    /// the new UI ignores them.
    pub harness_overrides: HashMap<String, HarnessConfigValue>,
    /// Per-mesh cap on **concurrent admitted circuit runs** (issue #1467)
    /// — see the matching [`Mesh`] field. Surface for the dedicated
    /// Autopilot Probe tab so the legacy `autopilot_concurrency_limit`
    /// (kept here for back-compat with the `update_mesh_autopilot`
    /// atomic write) and the new circuit-run gate appear side-by-side.
    #[ts(as = "i32")]
    pub circuit_run_capacity: i32,
    /// Per-Mesh Worktree Node directory override (issue #1519) — see
    /// the matching [`Mesh`] field. Surfaced in Project Settings →
    /// Worktrees so the Mesh can override the application default.
    pub worktree_directory: Option<String>,
}

impl From<&Mesh> for MeshRow {
    fn from(mesh: &Mesh) -> Self {
        Self {
            name: if mesh.name.is_empty() { None } else { Some(mesh.name.clone()) },
            build_command: mesh.build_command.clone(),
            run_command: mesh.run_command.clone(),
            model: mesh.model.clone(),
            effort: mesh.effort.clone(),
            base_ref: Some(mesh.base_ref.clone()),
            use_worktree: mesh.use_worktree,
            worktree_mode: mesh.worktree_mode.clone(),
            default_provider: mesh.default_provider.clone(),
            sandbox: mesh.sandbox,
            pre_spawn_pool_size: mesh.pre_spawn_pool_size,
            autopilot_enabled: mesh.autopilot_enabled,
            autopilot_trigger_label: mesh.autopilot_trigger_label.clone(),
            autopilot_concurrency_limit: mesh.autopilot_concurrency_limit,
            autopilot_provider: mesh.autopilot_provider.clone(),
            autopilot_action_on_success: mesh.autopilot_action_on_success.clone(),
            root_build_command: mesh.root_build_command.clone(),
            root_run_command: mesh.root_run_command.clone(),
            autopilot_mode: mesh.autopilot_mode,
            loop_initial_prompt: mesh.loop_initial_prompt.clone(),
            loop_suffix_prompt: mesh.loop_suffix_prompt.clone(),
            loop_max_iterations: mesh.loop_max_iterations,
            loop_interval_seconds: mesh.loop_interval_seconds,
            loop_consecutive_failures: mesh.loop_consecutive_failures,
            harness_overrides: mesh.harness_overrides.clone(),
            circuit_run_capacity: mesh.circuit_run_capacity,
            worktree_directory: mesh.worktree_directory.clone(),
        }
    }
}
