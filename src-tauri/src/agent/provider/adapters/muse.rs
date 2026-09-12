//! Muse Code 1.1.1 interactive CLI contract, checked against the installed
//! Linux CLI. See docs/learning/windows-wsl-harness-interop.md.
//!
//! **Approval policy (issue #1705).** Every interactive harness Buildmesh
//! spawns runs unattended in a PTY, so each adapter bakes its harness's
//! "don't block on approval prompts" policy into `spawn_recipe`. Muse offers
//! three CLI knobs for that policy:
//!
//! - `--approval-mode <untrusted|on-request|never>` — explicit mode
//!   (default `on-request`).
//! - `--disable-approval` — disables tool approval only; outer sandbox stays
//!   on. Sibling-harness precedent: matches OpenCode `--auto` and
//!   AGY / Claude `--dangerously-skip-permissions` in spirit (one flag, one
//!   policy, adapter-owned).
//! - `--yolo` — disables approval **AND** sandboxing **AND** trusts the
//!   workspace. Three policies in one. Explicitly rejected by issue #1705 as
//!   too wide for the quiet default.
//!
//! The chosen policy is **`--disable-approval`** (maintainer decision,
//! issue #1705). `--yolo` is never baked in.
//!
//! **Attention (issue #1709).** Muse exposes no interactive attention-hook
//! registration: `muse --help` has no hook/event flag, there is no workspace
//! or global hook config file, and the MSP lifecycle events (`turn/*`,
//! `approval/*`, `userInput/*` in `muse schema generate-json-schema`) are
//! served only on the separate `muse serve` stdio plane — the headless
//! architecture Buildmesh's PTY spawn does not use.
//!
//! The interactive TUI does, however, append run boundaries to its durable
//! session log (`~/.local/share/muse/sessions/YYYY/MM/DD/<uuid>/session.jsonl`).
//! Muse therefore supplies its turn signal through the passive watcher
//! (`services::muse_watcher`), mirroring Command Code: `requires_attention_hook`
//! stays `false` and `attention_capability` stays `None`.
//!
//! **Launch mode is `SkipPermissions`.** With `--disable-approval` the harness
//! never raises a tool-approval prompt — every observed `approval_disabled`
//! session carries zero `approval/requested` records — so a
//! `PermissionRequested` lifecycle signal is impossible by construction and is
//! deliberately not classified. A `run/terminal` record yields the node back
//! to the user; `terminal` is `completed | failed | cancelled`.
use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct MuseAdapter;
pub static MUSE: MuseAdapter = MuseAdapter;

impl AgentProvider for MuseAdapter {
    fn id(&self) -> &'static str {
        "muse"
    }
    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Meta Muse".into(),
            color: "#0866ff".into(),
            icon: "M".into(),
        }
    }
    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        // Issue #1705: bake `--disable-approval`. See the module docstring
        // for the rationale (sibling-harness precedent; `--yolo` rejected
        // as too wide; outer sandbox stays on).
        SpawnRecipe {
            binary: "muse",
            base_args: vec!["--disable-approval".into()],
            trailing_args: vec![],
            windows_shell: WindowsShell::Direct,
        }
    }
    fn supports_resume(&self) -> bool {
        true
    }
    fn auto_resume_on_startup(&self) -> bool {
        true
    }
    fn self_assigns_session_id(&self) -> bool {
        true
    }
    fn captures_session_id_from_pty(&self) -> bool {
        false
    }
    fn requires_attention_hook(&self) -> bool {
        false
    }
    // Issue #1709: no native hook exists, so Muse's turn signal comes from the
    // backend-owned session-log watcher instead.
    fn supports_passive_turn_watcher(&self) -> bool {
        true
    }
    fn on_spawn_activated(&self, node_id: i64) {
        crate::services::muse_watcher::activate(node_id);
    }
    fn on_process_terminated(&self, node_id: i64) {
        crate::services::muse_watcher::stop(node_id);
    }
    fn produces_readable_transcript(&self) -> bool {
        // Issue #1708: the muse reader
        // (`services::transcript_reader::adapters::muse::MuseAdapter`) is
        // wired, so muse nodes now hydrate the Coordinator Node Digest's
        // rich layer AND surface in the archived-node resume picker
        // (the `resumable = supports_resume && produces_readable_transcript`
        // conjunction in `provider_menu.rs:53`).
        true
    }
    fn supports_model_override(&self) -> bool {
        true
    }
    fn supports_extra_args(&self) -> bool {
        true
    }
    fn supports_prefill(&self) -> bool {
        true
    }
    // Resume accepts a session UUID but does not document a positional prompt.
    fn prefill_requires_pty(&self, _text: &str) -> bool {
        true
    }
    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Linux, Platform::Macos]
    }
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["resume".into(), id.into()]
    }
    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }

    fn recover_suspended_session_id(
        &self,
        spawn_path: &str,
        _env_type: EnvType,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        let native = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(std::path::PathBuf::from)?
            .join(".local/share/muse");
        let home = crate::env::cli_dir_for_spawn(native, ".local/share/muse", spawn_path)?;
        find_session(
            &home.join("session-index.db"),
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        _env_type: EnvType,
        app: &tauri::AppHandle,
    ) {
        let spawn_path = spawn_path.to_string();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            for delay in [200, 500, 1000, 2000, 4000, 8000] {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                    break;
                }
                let result = crate::blocking::run_blocking("muse_session_capture", move || {
                    crate::services::session_recovery::recover_live_node(node_id)
                })
                .await;
                let Ok(Some(session_id)) = result else {
                    continue;
                };
                // Capture is the watcher's arm point: the same session index
                // that supplied the id also resolves the log path.
                let spawn_path = spawn_path.clone();
                let app = app.clone();
                let started = crate::blocking::run_blocking("muse watcher start", move || {
                    crate::services::muse_watcher::start_for_session(
                        node_id,
                        &session_id,
                        &spawn_path,
                        &app,
                    )
                })
                .await;
                match started {
                    Ok(()) => break,
                    // The id is durable once captured, so a transient log-path
                    // failure (e.g. a not-yet-visible WSL file) retries on the
                    // next tick instead of stranding the watcher.
                    Err(error) => {
                        tracing::warn!("muse watcher: could not start for node {node_id}: {error}")
                    }
                }
            }
        });
    }

    fn before_resume_spawn<'a>(
        &'a self,
        node_id: i64,
        session_id: &str,
        spawn_path: &str,
        _env_type: EnvType,
        app: &'a tauri::AppHandle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        let session_id = session_id.to_string();
        let spawn_path = spawn_path.to_string();
        let app = app.clone();
        Box::pin(async move {
            if let Err(error) = crate::services::muse_watcher::start_for_resumed_session_async(
                node_id,
                &session_id,
                &spawn_path,
                app,
            )
            .await
            {
                tracing::warn!("muse watcher: could not resume watch for node {node_id}: {error}");
            }
        })
    }
}

fn find_session(
    database: &std::path::Path,
    workspace: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    connection
        .busy_timeout(std::time::Duration::from_millis(200))
        .ok()?;
    // Muse's index can leave workspace/timestamp columns NULL. Read only
    // the session metadata frame from the indexed log, never transcript text.
    let mut statement = connection.prepare("SELECT session_id, session_log_path FROM sessions ORDER BY session_log_path DESC LIMIT 128").ok()?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok()?;
    let indexed: Vec<_> = rows.filter_map(Result::ok).collect();
    drop(statement);
    drop(connection);
    let candidates = indexed.into_iter().filter_map(|(id, path)| {
        let path = crate::env::to_host_path(&path);
        let (recorded_id, cwd, timestamp) = session_metadata(std::path::Path::new(&path))?;
        (id == recorded_id && crate::env::directories_match(&cwd, workspace))
            .then_some((id, timestamp))
    });
    crate::services::session_recovery::select_recovery_identity(
        candidates,
        anchor_ms,
        recorded_start,
    )
}

fn session_metadata(path: &std::path::Path) -> Option<(String, String, i64)> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file.take(262_144)).lines().take(64) {
        let line = line.ok()?;
        let frame: serde_json::Value = serde_json::from_str(&line).ok()?;
        if let Some(metadata) = metadata_record(&frame) {
            return Some(metadata);
        }
        let Some(children) = frame.get("children").and_then(|c| c.as_array()) else {
            continue;
        };
        for child in children {
            let Some(json) = child.get("record_json").and_then(|r| r.as_str()) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<serde_json::Value>(json) else {
                continue;
            };
            if let Some(metadata) = metadata_record(&record) {
                return Some(metadata);
            }
        }
    }
    None
}

fn metadata_record(record: &serde_json::Value) -> Option<(String, String, i64)> {
    if record.get("payload_type").and_then(|p| p.as_str()) != Some("runtime.session.metadata")
        || record.pointer("/stream/kind").and_then(|s| s.as_str()) != Some("session")
    {
        return None;
    }
    let id = record.pointer("/stream/id")?.as_str()?;
    uuid::Uuid::parse_str(id).ok()?;
    let cwd = record.pointer("/payload/record/workspace_root")?.as_str()?;
    let timestamp = record.get("recorded_at")?.as_i64()?;
    Some((id.into(), cwd.into(), timestamp / 1000))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Pick the canonical `EnvType` that pairs with each `Platform` the
    /// muse adapter advertises on. Keeps the per-platform pin test readable
    /// as "this is the host that runs the binary" rather than a sloppy
    /// single `EnvType::Wsl` everywhere (issue #1705 round-1 review —
    /// `Platform::Macos, EnvType::Wsl` is a platform-impossible pairing).
    /// Mirrors the pairing OpenCode's `spawn_recipe_direct_on_macos` test
    /// uses: macOS has no WSL runtime, and `EnvType` has no `Macos`
    /// variant, so the canonical macOS pairing is `(Macos, Windows)`.
    fn env_type_for(platform: Platform) -> EnvType {
        match platform {
            // Linux runtime covers native Linux + WSL-on-Windows (the
            // Ubuntu distro at `/home/alond/.local/bin/muse`). The muse
            // adapter's `spawn_recipe` is platform-agnostic, but the
            // pairing keeps the test accurate as "the host that runs the
            // binary" rather than a meaningless one-size-fits-all value.
            Platform::Linux => EnvType::Wsl,
            // macOS has no WSL runtime and no `EnvType::Macos` variant; the
            // canonical macOS pairing is `(Macos, Windows)`, matching
            // OpenCode's `spawn_recipe_direct_on_macos` test pattern.
            Platform::Macos => EnvType::Windows,
            Platform::Windows => EnvType::Windows,
        }
    }

    #[test]
    fn muse_uses_documented_interactive_arguments() {
        assert_eq!(
            MUSE.spawn_recipe(Platform::Linux, EnvType::Wsl).binary,
            "muse"
        );
        assert_eq!(MUSE.resume_args("session-uuid"), ["resume", "session-uuid"]);
        assert_eq!(MUSE.prefill_args("fix the bug"), ["fix the bug"]);
        assert_eq!(MUSE.model_args("model-id"), ["--model", "model-id"]);
        assert!(!MUSE.captures_session_id_from_pty());
        assert!(MUSE.prefill_requires_pty("follow-up"));
        // The baked `--disable-approval` policy (issue #1705) is pinned
        // exhaustively by `spawn_recipe_carries_disable_approval_on_supported_platforms`
        // below — that test iterates every supported host and adds the
        // `--yolo` negative assertion. Keeping the assertion only there
        // keeps the named domain of this test (per-adapter `*_args` shape)
        // focused.
    }

    /// Issue #1705 — per-platform pin of the baked approval flag.
    /// Mirrors OpenCode's `spawn_recipe_carries_auto_flag_on_every_platform`:
    /// iterate over `available_on()` (not every `Platform` variant — muse
    /// does not run on Windows) and assert the exact base_args vector so
    /// a future flag smuggle (e.g. `--approval-mode never` slipping in
    /// alongside `--disable-approval`) trips here, not at runtime.
    ///
    /// Each platform variant is paired with the canonical `EnvType` for
    /// that host (see [`env_type_for`]) so the test reads as "the host
    /// that actually runs the binary" — `Platform::Macos, EnvType::Wsl`
    /// would be a platform-impossible pairing.
    #[test]
    fn spawn_recipe_carries_disable_approval_on_supported_platforms() {
        for platform in MUSE.available_on() {
            let recipe = MUSE.spawn_recipe(*platform, env_type_for(*platform));
            assert_eq!(
                recipe.binary, "muse",
                "muse binary name must be exact on {platform:?}"
            );
            assert_eq!(
                recipe.base_args,
                vec!["--disable-approval".to_string()],
                "muse base recipe must be exactly `[\"--disable-approval\"]` \
                 on {platform:?}; got {:?}",
                recipe.base_args
            );
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "muse is a real ELF binary / macOS Mach-O on its supported \
                 hosts — must use WindowsShell::Direct on {platform:?}; got {:?}",
                recipe.windows_shell
            );
            // `--yolo` is the explicit no-go for issue #1705: it disables
            // approval AND sandboxing AND trusts the workspace. A future
            // "while we're here" edit that adds it would silently widen the
            // policy beyond the maintainer-approved scope.
            assert!(
                !recipe.base_args.iter().any(|a| a == "--yolo"),
                "muse base recipe must never bake --yolo (issue #1705): \
                 it disables approval + sandboxing + workspace trust in one \
                 flag and was explicitly rejected; got {:?}",
                recipe.base_args
            );
        }
    }

    // -- Prepared-launch evidence (issue #1705 round-1 review) ------------
    //
    // The per-platform pin above proves `spawn_recipe()` itself returns the
    // baked policy; it does not prove the policy survives `default_prepare`
    // composition. The two tests below route fresh + resume launches through
    // the real orchestration seam (`agent::launch::default_prepare`) so the
    // baked flag is proven to land in the final argv alongside the model
    // override, the prefill text, and the resume id, in the documented order.
    // Without these, a future refactor that reorders the layers (e.g.
    // prepending `--model` before `--disable-approval`) would slip past the
    // per-platform pin but break a real spawn.
    //
    // Mirrors OpenCode's `fresh_recipe_forwards_model_and_prompt_without_session_id`
    // and `resume_recipe_carries_session_flag` — the engineering contract
    // (`docs/agents/engineering.md`) requires testing fresh AND resume paths
    // for changed launch recipes.

    /// Issue #1705 fresh launch: the baked `--disable-approval` must land
    /// ahead of the model override and the prefill text in the final argv.
    /// Pin the exact `base_args` vector so a future reorder that pushes
    /// `--disable-approval` past `--model` (or drops it during layer
    /// composition) trips here, not in production.
    #[test]
    fn default_prepare_fresh_launch_carries_disable_approval_with_model_and_prefill() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("claude-sonnet-4-5".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Wsl,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("fix the auth bug"),
            sandbox: false,
        };
        let prepared = default_prepare(&MUSE, input);
        // Order is `base_recipe -> model -> prefill`. muse's `prefill_args`
        // returns a positional element (no `--prefill` flag — see
        // `muse_uses_documented_interactive_arguments`), so prefill lands
        // as a bare trailing argv element after the model flag+value.
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--disable-approval".to_string(),
                "--model".to_string(),
                "claude-sonnet-4-5".to_string(),
                "fix the auth bug".to_string(),
            ],
            "fresh launch argv must keep --disable-approval ahead of --model \
             and the prefill text; got {:?}",
            prepared.recipe.base_args
        );
        // Negative guards: no session-assign flag (muse self-assigns), no
        // `--prefill` flag (the prefill shape is positional), no
        // approval-policy smuggle (`--yolo` was explicitly rejected).
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--session"
                || a == "--session-id"
                || a == "--prefill"
                || a == "--yolo"),
            "fresh launch must not emit session-assign / --prefill / --yolo; \
             got {:?}",
            prepared.recipe.base_args
        );
    }

    /// Issue #1705 resume launch: the baked `--disable-approval` must land
    /// ahead of the resume subcommand + session id, matching the order the
    /// OpenCode adapter uses for `--auto --session <id>`. Pin the exact
    /// vector so a future edit that orders the resume subcommand before
    /// the baked flag (or that drops the flag during composition) trips
    /// here.
    #[test]
    fn default_prepare_resume_launch_carries_disable_approval_then_resume_uuid() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Wsl,
            session: SessionIdModeRef::Resume("12345678-1234-4234-8234-123456789abc"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&MUSE, input);
        assert_eq!(
            prepared.recipe.base_args,
            vec![
                "--disable-approval".to_string(),
                "resume".to_string(),
                "12345678-1234-4234-8234-123456789abc".to_string(),
            ],
            "resume launch argv must be exactly \
             `--disable-approval resume <uuid>`; got {:?}",
            prepared.recipe.base_args
        );
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--yolo"),
            "resume launch must never bake --yolo (issue #1705); got {:?}",
            prepared.recipe.base_args
        );
    }

    #[test]
    fn muse_session_index_matches_workspace_and_refuses_ambiguous_identity() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("session-index.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("CREATE TABLE sessions (session_id TEXT, session_log_path TEXT);")
            .unwrap();
        let insert = |id: &str, workspace: &str, timestamp: i64, wrapped: bool| {
            let log = directory.path().join(format!("{id}.jsonl"));
            let record = serde_json::json!({"payload_type": "runtime.session.metadata", "stream": {"kind":"session", "id":id}, "recorded_at":timestamp, "payload":{"record":{"workspace_root":workspace}}});
            let frame = if wrapped {
                serde_json::json!({"retained_frame":"session_permission_transaction","children":[{"record_json":record.to_string()}]})
            } else {
                record
            };
            std::fs::write(&log, format!("{{}}\n{frame}\n")).unwrap();
            connection
                .execute(
                    "INSERT INTO sessions VALUES (?1, ?2)",
                    rusqlite::params![id, log.to_str().unwrap()],
                )
                .unwrap();
        };
        insert(
            "12345678-1234-4234-8234-123456789abc",
            "/workspace",
            10000000,
            false,
        );
        insert(
            "22345678-1234-4234-8234-123456789abc",
            "/other",
            10000000,
            true,
        );
        assert_eq!(
            find_session(&database, "/workspace", 10000, true).as_deref(),
            Some("12345678-1234-4234-8234-123456789abc")
        );
        assert_eq!(find_session(&database, "/workspace", 20000, true), None);
        insert(
            "32345678-1234-4234-8234-123456789abc",
            "/workspace",
            11000000,
            true,
        );
        assert_eq!(find_session(&database, "/workspace", 10000, true), None);
    }
}
