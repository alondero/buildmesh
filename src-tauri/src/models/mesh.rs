//! Mesh and Autopilot-mode wire types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;



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
    /// Per-mesh cap on concurrent admitted Circuit runs, independent of agent fan-out.
    /// Defaults to 2; validated to 1..=8 by `update_mesh_circuit_run_capacity`.
    /// Persisted as `meshes.circuit_run_capacity` (schema v36).
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
    /// Per-context build/run commands (issue #802). When set, a Root Node
    /// runs these instead of `build_command` / `run_command`; `None` falls
    /// back to those. See the matching [`Mesh`] fields.
    pub root_build_command: Option<String>,
    pub root_run_command: Option<String>,
    /// Circuit run capacity displayed in the Circuits Probe; see the matching `Mesh` field.
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

            root_build_command: mesh.root_build_command.clone(),
            root_run_command: mesh.root_run_command.clone(),

            circuit_run_capacity: mesh.circuit_run_capacity,
            worktree_directory: mesh.worktree_directory.clone(),
        }
    }
}
