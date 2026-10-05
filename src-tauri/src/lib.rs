//! Buildmesh — Rust Backend
//! AI Agent Orchestration Hub for Windows + WSL

pub mod agent;
mod attention_autoclear;
mod blocking;
pub mod circuit;
mod commands;
mod coordinator;
mod db;
mod diagnostics;
mod env;
mod git;
mod http;
mod http_server;
mod instance_guard;
pub mod models;
mod node_turn;
mod preferences;
pub mod process_util;
mod pty;
mod review_contract;
pub mod sandbox;
pub mod secret_scrubber;
mod services;
mod session_capture;
mod session_naming;
mod startup;
mod windowing;

use tauri::Manager;

/// Handle the private external-crash-watchdog CLI before Tauri initialises.
pub fn run_crash_watchdog_if_requested() -> bool {
    diagnostics::run_crash_watchdog_if_requested()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Early panic hook — captures panics that fire BEFORE the main panic
    // hook is installed later in `setup` (Tauri setup can panic on misconfig,
    // and those panics would otherwise die with only the truncated "disabled
    // backtrace" tail in `panic.log`). Writes to a separate
    // `panic_early.log` so it doesn't interleave with the main hook's output,
    // and syncs to disk so the message survives `panic = "abort"` killing
    // the process via __fastfail before the OS file buffer flushes.
    //
    // Cross-platform + dev-profile aware: derives the bundle id from the binary
    // name (buildmesh.exe → stable, buildmesh-dev.exe → dev) so dev-profile
    // crashes go to their own `com.alond.buildmesh.dev` dir, not the stable
    // hub's. macOS/Linux resolve `$HOME` / `$XDG_DATA_HOME` instead of APPDATA.
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;

        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic".to_string()
        };
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let line = format!(
            "[{}] msg={} loc={}",
            chrono::Utc::now().to_rfc3339(),
            msg,
            loc
        );
        // Always echo to stderr — if the file write fails (no APPDATA, no
        // HOME, missing parent dir, permission), the panic payload is at
        // least visible in the launcher's terminal.
        eprintln!("{}", line);

        // Derive the bundle id from the running binary name so dev-profile
        // panics don't pollute the stable profile's logs (and vice versa).
        // `cargo tauri:build:dev` emits `buildmesh-dev.exe`; the stable CLI
        // emits `buildmesh.exe`.
        let bundle_id = std::env::args()
            .next()
            .map(|a| {
                let leaf = std::path::Path::new(&a)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if leaf.contains("dev") {
                    "com.alond.buildmesh.dev"
                } else {
                    "com.alond.buildmesh"
                }
            })
            .unwrap_or("com.alond.buildmesh");

        // Per-platform data dir: APPDATA on Windows, XDG_DATA_HOME (fallback
        // ~/.local/share) on Linux, $HOME/Library/Application Support on macOS.
        let data_dir: Option<std::path::PathBuf> = if cfg!(target_os = "windows") {
            std::env::var_os("APPDATA").map(std::path::PathBuf::from)
        } else if let Some(home) = std::env::var_os("HOME") {
            let home = std::path::PathBuf::from(home);
            if cfg!(target_os = "macos") {
                Some(home.join("Library").join("Application Support"))
            } else {
                Some(
                    std::env::var_os("XDG_DATA_HOME")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| home.join(".local").join("share")),
                )
            }
        } else {
            None
        };

        if let Some(dir) = data_dir {
            let log_path = dir.join(bundle_id).join("logs").join("panic_early.log");
            // Create the parent dirs — `OpenOptions::create(true)` only
            // creates the file, not the path. On a fresh install neither
            // `%APPDATA%\com.alond.buildmesh\` nor its `logs/` subdir exist
            // when the first panic fires.
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                let _ = writeln!(f, "{}", line);
                // `flush()` + `sync_all()`: the hook runs to completion
                // BEFORE `panic = "abort"` calls `__fastfail` to terminate
                // the process, so both syscalls return normally — `sync_all`
                // is the durability guarantee that survives the abrupt
                // termination. Drop them only if you also drop `panic=abort`.
                let _ = f.flush();
                let _ = f.sync_all();
            }
        }
    }));

    tauri::Builder::default()
        .manage(crate::services::gh_auth_cache::GhAuthCache::new())
        // Durable last-known Usage Meter readings (ADR-0037). Resolves the app
        // data dir lazily per call, since `manage` runs before `setup()`.
        .manage(crate::services::usage::last_known::UsageLastKnownCache::new())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        // In-app auto-update (issue #826). `updater` checks the GitHub Releases
        // feed configured in tauri.conf.json and verifies each package against
        // the committed minisign pubkey; `process` provides the `relaunch()`
        // the frontend calls after `downloadAndInstall()`.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // Issue #1525 — diagnostics come *before* the database. Resolving
            // the app-data profile and opening the bounded `buildmesh.log`
            // used to happen inside `run_profile_startup` alongside (and
            // therefore after) `db::init`, so every failure most likely to
            // stop the app opening left no window, no message, and no log.
            //
            // `AppHandle` rather than `&App` so the retry closure below can
            // call back in without holding a borrow of the whole app.
            let handle = app.handle().clone();
            let bootstrap = match startup::bootstrap(&handle) {
                Ok(bootstrap) => bootstrap,
                Err(failure) => {
                    // The only stages that can fail this early are the
                    // retry-safe ones — nothing process-global is installed
                    // yet — so the user gets a real second attempt rather
                    // than a button that cannot work.
                    let outcome = {
                        let mut retry = || startup::bootstrap(&handle).map(|_| ());
                        startup::report_with_retry(failure, &mut retry)
                    };
                    match outcome {
                        // The user's retry got past it; carry on with the
                        // normal startup.
                        Ok(()) => startup::installed()
                            .expect("a successful retry installs the bootstrap"),
                        // The user quit with the failure outstanding. It is
                        // already recorded and already on screen, so this
                        // only has to end the process.
                        Err(failure) => {
                            return Err(Box::new(failure) as Box<dyn std::error::Error>)
                        }
                    }
                }
            };

            // One Buildmesh process per app-data profile (issue #1521). The
            // claim is taken before `db::init` and before any worker starts,
            // and the startup body runs only for the winner — see
            // `instance_guard` for why a second process must reach neither.
            // The `.dev` profile is a different profile, so a dev build still
            // runs alongside a stable install.
            let app_dir = bootstrap.profile_dir().to_path_buf();
            let identifier = app.config().identifier.clone();
            let identity = match instance_guard::ProfileIdentity::new(&identifier, &app_dir) {
                Ok(identity) => identity,
                Err(error) => return Err(ownership_failure(&identifier, &app_dir, error)),
            };

            match instance_guard::with_profile_ownership(
                &identity,
                |report| {
                    if let Err(e) =
                        instance_guard::record_forwarded(&app_dir, &identifier, report)
                    {
                        eprintln!("could not record the forwarded launch: {e}");
                    }
                },
                || run_profile_startup(app, &identity, bootstrap),
            ) {
                Ok(instance_guard::Startup::Owned) => Ok(()),
                // The profile belongs to a live process: its window was asked
                // to come forward, and the startup continuation never ran.
                Ok(instance_guard::Startup::Forwarded(_)) => std::process::exit(0),
                Err(instance_guard::StartupError::Ownership(error)) => {
                    Err(ownership_failure(&identifier, &app_dir, error))
                }
                // The continuation already reported the failure through the
                // native error surface, so this only propagates it.
                Err(instance_guard::StartupError::Body(error)) => Err(error),
            }
        })
        .invoke_handler(tauri::generate_handler![
            // Agent Node (issue #490: renamed from `*_session` to `*_agent_node`).
            commands::agent_node::create_agent_node,
            commands::agent_node::list_agent_nodes,
            commands::agent_node::list_agent_history,
            commands::agent_node::reopen_agent_node,
            commands::agent_node::get_agent_node,
            commands::agent_node::delete_agent_node,
            commands::agent_node::get_worktree_close_safety,
            commands::agent_node::rename_agent_node,
            commands::agent_node::update_agent_node_positions,
            commands::agent_node::regenerate_agent_node,
            // Node Pinning (wayfinder #982 / ticket #984): the persisted
            // backing storage for the Pinned Grid view mode (#986). The UI
            // affordance (#985) calls these to flip / set a node's
            // `is_pinned` flag, and the view switcher (#983) reads it back
            // through `list_agent_nodes`.
            commands::agent_node::set_node_pinned,
            commands::agent_node::toggle_node_pinned,
            // Mesh (renamed from `*_project` to `*_mesh`).
            commands::mesh::add_mesh,
            commands::mesh::pick_mesh_folder,
            commands::mesh::create_mesh,
            commands::mesh::clone_mesh_repo,
            commands::mesh::update_mesh_color,
            commands::mesh::create_test_mesh,
            commands::mesh::list_meshes,
            commands::mesh::delete_mesh,
            commands::mesh::update_mesh_layout,
            commands::mesh::update_mesh_positions,
            commands::mesh::get_root_token,
            commands::mesh::get_local_ip,
            commands::mesh::get_default_provider,
            // App preferences (buildmesh-wide)
            commands::preferences::get_app_preferences,
            // Corrupt-file recovery (issue #1523)
            commands::preferences::get_preferences_health,
            commands::preferences::restore_preferences_backup,
            commands::preferences::reset_app_preferences,
            commands::preferences::open_preferences_location,
            commands::preferences::set_app_default_provider,
            commands::preferences::set_app_reviewer_provider,
            commands::preferences::set_app_naming_provider,
            commands::preferences::set_app_circuit_agent_pool_size,
            commands::preferences::set_circuit_classifier_provider,
            commands::preferences::set_app_confirm_before_quit,
            commands::preferences::get_probe_spawn_prompt_defaults,
            commands::preferences::set_app_issue_spawn_prompt,
            commands::preferences::set_app_pr_spawn_prompt,
            commands::preferences::set_harness_order,
            commands::preferences::set_proxied_provider_order,
            commands::preferences::get_provider_accounts,
            commands::preferences::get_keyed_first_class_catalog,
            commands::preferences::upsert_provider_account,
            commands::preferences::remove_provider_account,
            commands::preferences::get_provider_pairings,
            commands::preferences::get_pairing_verifications,
            commands::preferences::verify_provider_pairing,
            commands::preferences::get_pairing_defaults,
            commands::preferences::compatible_providers_for_harness,
            commands::preferences::compatible_providers_by_harness,
            commands::preferences::attach_proxied_provider,
            commands::preferences::update_provider_pairing,
            commands::preferences::remove_provider_pairing,
            // Application-level Agent Harness defaults (issue #1150 / #1148):
            // sparse map keyed by stable harness profile id, validated
            // against the capability descriptor (issue #1148 AC #5).
            commands::preferences::set_harness_default,
            commands::preferences::clear_harness_default,
            preferences::spawn_configurations::list_spawn_configurations,
            preferences::launch_catalog::get_provider_catalogue,
            preferences::launch_catalog::get_launch_targets,
            preferences::spawn_configurations::save_spawn_configuration,
            preferences::spawn_configurations::verify_launch_configuration,
            preferences::spawn_configurations::delete_spawn_configuration,
            // Resolved harness view (issue #1656) — single IPC entry that
            // returns the full per-harness cascade (four-layer breakdown +
            // capability-masked resolved value) for the Settings modal and
            // the spawn menu. Calls into the shared
            // `preferences::resolver::cascade` helper so the cascade
            // order lives in exactly one place (the spawn path also routes
            // through the same helper).
            commands::preferences::get_resolved_harness_view,
            // Configurable Worktree Node directories (issue #1519): app-wide
            // default + effective-dir read for the Settings surfaces.
            commands::preferences::set_app_worktree_directory,
            commands::preferences::get_worktree_directory_config,
            // Coordinator read API (ADR-0008)
            commands::coordinator::get_coordinator_status,
            commands::coordinator::set_coordinator_api_enabled,
            commands::coordinator::generate_coordinator_read_token,
            commands::coordinator::set_coordinator_drive_enabled,
            commands::coordinator::generate_coordinator_drive_token,
            // Authorized devices (issue #502)
            commands::devices::list_device_sessions,
            commands::devices::create_pairing_ticket,
            commands::devices::revoke_device_session,
            // Network exposure (issue #501)
            commands::network::get_network_status,
            commands::network::set_lan_exposure_enabled,
            // Cert status (issue #635) — QR-modal fingerprint surface so a
            // user whose installed root CA is stale can see "your cert is X,
            // server is now Y" without reaching for openssl.
            commands::network::get_cert_chain_status,
            // Root CA bytes for the QR-modal one-tap phone install (issue
            // #702). Same disk read as the /install-cert.der route, encoded
            // as base64 for embedding in a data: URL the phone's OS CA
            // installer intercepts.
            commands::network::get_root_cert_der,
            // Signed `.mobileconfig` profile for the iOS install-QR (issue
            // #713). DER-encoded PKCS#7/CMS SignedData wrapping the
            // unsigned Apple Configurator 2 plist, base64-encoded — the
            // frontend prepends `data:application/x-apple-aspen-config;base64,`
            // to produce the data: URL Safari intercepts.
            commands::network::get_root_cert_mobileconfig,
            // Explicit root reset (issue #1527) — wipes the persisted
            // TLS state so the next bind mints a fresh root. Frontend
            // exposes this as a Settings "Reset trusted certificates"
            // affordance; never auto-invoked from bind paths.
            commands::network::reset_trusted_certificates,
            // State recovery (issue #1537) — Settings > Data & Diagnostics.
            // Snapshot, integrity check, redacted export, and staged restore
            // of the profile's durable state. `stage_state_restore` only
            // verifies + stages; `run_profile_startup` applies it on the next
            // launch, before any connection or worker exists.
            commands::state_recovery::get_state_recovery_info,
            commands::state_recovery::list_state_snapshots,
            commands::state_recovery::create_state_snapshot,
            commands::state_recovery::check_state_integrity,
            commands::state_recovery::export_state,
            commands::state_recovery::export_state_to,
            commands::state_recovery::inspect_state_bundle,
            commands::state_recovery::stage_state_restore,
            commands::state_recovery::cancel_state_restore,
            commands::state_recovery::get_state_data_folder,
            // Agent
            // Process-lifecycle Tauri commands (issue #1052) live in
            // `agent::process`; the rest are spawn orchestration owned by
            // `commands::agent`.
            commands::agent::spawn_agent,
            agent::process::resize_agent,
            agent::process::kill_agent,
            agent::process::is_agent_running,
            agent::process::debug_list_agents,
            agent::process::send_to_agent,
            agent::process::write_to_agent,
            // "Handover to node" — stage the selection in an existing node's
            // PTY and submit it (context menu, sibling of spawn_handover_agent).
            agent::process::handover_to_agent,
            // Binary PTY output Channel (issue #1385). Complements the
            // `agent-output` event fallback; see `agent::output`.
            agent::output::subscribe_agent_output,
            agent::output::unsubscribe_agent_output,
            commands::agent::auto_resume_agent_nodes,
            agent::process::debug_crash_snapshot,
            agent::provider_menu::list_providers,
            agent::provider_menu::list_routing_options,
            commands::agent::spawn_issue_agent,
            commands::agent::spawn_handover_agent,
            commands::agent::create_issue_node,
            // Autopilot Circuits (spec #1205 / walking skeleton #1206).
            commands::circuit::list_circuits,
            commands::circuit::list_circuit_agent_ownerships,
            commands::circuit::get_circuit,
            commands::circuit::list_circuits_with_runs,
            commands::circuit::list_circuit_queue,
            commands::circuit::list_circuit_probe,
            commands::circuit::create_circuit,
            commands::circuit::copy_review_blueprint,
            commands::circuit::set_circuit_enabled,
            commands::circuit::update_circuit_graph,
            commands::circuit::update_circuit_concurrency_limit,
            commands::circuit::delete_circuit,
            commands::circuit::cancel_circuit_run,
            commands::circuit::cancel_circuit_runs,
            commands::circuit::move_circuit_run,
            commands::circuit::reorder_circuit_queue,
            commands::circuit::trigger_circuit_now,
            commands::circuit::trigger_circuit_from_node,
            commands::circuit::list_circuit_runs,
            commands::circuit::circuit_run_history,
            commands::circuit::record_circuit_outcome,
            commands::circuit::pause_circuit_run,
            commands::circuit::resume_circuit_run,
            commands::circuit::continue_circuit_review,
            commands::circuit::approve_circuit_step,
            // Build/Run
            commands::build_run::build_run,
            commands::build_run::get_mesh_row,
            commands::build_run::close_build_run,
            commands::build_run::write_to_build_run,
            commands::build_run::resize_build_run,
            // Binary PTY output Channel (issue #1393). Complements the
            // `build-run-output-{sessionId}` event fallback.
            commands::build_run::subscribe_build_run_output,
            commands::build_run::unsubscribe_build_run_output,
            // Mesh properties
            commands::mesh_properties::get_mesh_properties,
            commands::mesh_properties::update_mesh_name,
            commands::mesh_properties::update_mesh_column,
            commands::mesh_properties::update_worktree_base_ref,
            commands::mesh_properties::remove_worktree_base_ref,
            commands::mesh_properties::update_mesh_use_worktree,
            commands::mesh_properties::update_mesh_sandbox,
            commands::mesh_properties::update_mesh_pool_size,
            commands::mesh_properties::get_mesh_pool_count,
            // Circuit-run capacity (issue #1467) — narrow single-column
            // write for the new `meshes.circuit_run_capacity` column.

            commands::mesh_properties::update_mesh_circuit_run_capacity,
            // Configurable Worktree Node directories (issue #1519): per-Mesh
            // override with same-environment validation for absolute paths.
            commands::mesh_properties::update_mesh_worktree_directory,
            // Scratch Pad (Probe Panel "📝 Scratch Pad" tab)
            commands::scratchpad::get_mesh_scratchpad,
            commands::scratchpad::set_mesh_scratchpad,
            // Project detection (presets)
            commands::project_detect::detect_mesh_project,
            // Diff
            commands::diff::diff_files,
            commands::diff::diff_file_against_head,
            commands::diff::diff_node_against_base,
            commands::diff::diff_node_file_against_base,
            commands::diff::node_changed_files,
            commands::diff::node_changed_summary,
            // File tree
            commands::file_tree::list_directory,
            commands::file_tree::open_in_editor,
            commands::file_tree::open_in_file_manager,
            commands::file_tree::get_user_config_dir,
            commands::file_tree::to_host_path,
            // Git
            commands::git::get_git_status,
            commands::git::get_git_branch_status,
            commands::git::get_git_summary,
            commands::git::get_default_branch,
            commands::git::git_sync,
            commands::git::get_mesh_health,
            commands::git::get_mesh_git_static,
            commands::git::restore_mesh_to_base,
            commands::git::free_base_branch,
            commands::git::stage_file,
            commands::git::revert_file,
            // Prune (branches & worktrees)
            commands::prune::get_git_prune_info,
            commands::prune::delete_branches,
            commands::prune::delete_worktrees,
            commands::prune::prune_remote_tracking,
            // File watcher (renamed from `watch_session` / `unwatch_session` in issue #490)
            commands::file_watcher::watch_agent_node,
            commands::file_watcher::unwatch_agent_node,
            // Clipboard (native read bypasses macOS WKWebView permission popup)
            commands::clipboard::read_clipboard,
            // Resolved absolute log locations (issue #1525) so the Boot Error
            // Panel can name a real path instead of "buildmesh.log".
            commands::diagnostics::get_diagnostic_paths,
            // Frontend log bridge
            commands::frontend_log::log_frontend,
            // Attention (renamed from `*_attention_session` to `*_attention_node`)
            commands::attention::register_attention_node,
            commands::attention::clear_attention_node,
            commands::attention::list_semantic_turns,
            commands::attention::is_attention_pending,
            // PR
            commands::pr::create_pr,
            commands::pr::merge_pr,
            commands::pr::get_current_branch,
            commands::pr::create_pr_for_mesh,
            commands::agent::create_pr_node,
            commands::pr::get_repo_issues,
            commands::pr::get_open_pr_for_node,
            commands::pr::get_repo_pulls,
            commands::pr::get_pr_mergeability,
            commands::pr::get_prs_mergeability,
            commands::pr::get_pr_files,
            commands::pr::get_github_url_for_mesh,
            // General GitHub auth (issue #433 — moved out of `commands::pr`:
            // no PR call sites, used by git/mobile/UI auth checks).
            commands::github::check_gh_auth,
            commands::github::get_repo_labels,
            commands::github::set_issue_label,
            // App-level metadata (issue #826). The frontend uses
            // `get_app_identifier` to guard the in-app updater: the dev
            // profile (`com.alond.buildmesh.dev`) must not poll the stable
            // release feed, since `tauri:build:dev` is also a production Vite
            // build and a simple `import.meta.env.PROD` check can't tell
            // them apart.
            commands::app::get_app_identifier,
            // Exit-confirmation cancel (issue #1501): retracts the eager
            // `CloseRequested` expected-exit marking when the user backs
            // out of the modal, so the watchdog still relaunches on a
            // later real crash.
            commands::app::cancel_window_close,
            // Confirmed exit (issue #1501): lifecycle-owned shutdown from the
            // modal's "Exit Buildmesh", not an ACL-gated window IPC.
            commands::app::exit_application,
            // Native Snap Layouts for the bespoke title bar (ADR-0035). The
            // frontend reports the measured maximise-button box; a no-op off
            // Windows. Not ACL-gated, like the two window-lifecycle commands
            // above — event listening needs only `core:default`.
            commands::app::set_titlebar_maximize_metrics,
            // AI context portability
            commands::ai_context::detect_ai_context,
            commands::ai_context::create_ai_context_portability_pr,
            // Remote
            commands::remote::submit_terminal_snapshot,
            // Agent Node Discovery (renamed from `session_discovery` to `agent_node_discovery`)
            commands::agent_node_discovery::discover_agent_nodes,
            commands::agent_node_discovery::import_discovered_agent_node,
            // Usage
            commands::usage::get_provider_meters,
            // Observed Muse session telemetry (issue #1680). Node-scoped MSP
            // counters — not a Usage Meter and never remaining account quota.
            commands::muse::get_muse_session_telemetry,
            commands::usage::set_minimax_api_key,
            // OpenCode OAuth (issue #956 + #969). Device Flow + workspace
            // enumeration + token persistence seams the React Settings UI
            // (`OpenCodeAccountCard`) drives. Stateless-server design —
            // React owns the polling state; each call is one round-trip.
            commands::opencode_oauth::start_device_flow_console,
            commands::opencode_oauth::poll_opencode_device_token,
            commands::opencode_oauth::list_opencode_workspaces,
            commands::opencode_oauth::persist_opencode_tokens,
            commands::opencode_oauth::revoke_opencode_console,
            commands::opencode_oauth::get_opencode_console_status,
            commands::opencode_oauth::set_opencode_console_workspace,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            match &event {
                tauri::RunEvent::ExitRequested { code, .. } => {
                    diagnostics::mark_expected_exit(diagnostics::ExpectedExitReason::ExitRequested);
                    tracing::info!(
                        "App exit requested (code={:?}), marking running sessions as suspended",
                        code
                    );
                    // Routes through SessionLifecycle (issue #132, #949) —
                    // single owner of every `agent_nodes.status` write,
                    // including the exit-time suspend sweep. `on_exit_sweep()`
                    // wraps `db::mark_running_nodes_suspended()` exactly the
                    // same way `recover_from_crash()` does for the startup
                    // path; the wrappers are named separately so the trigger
                    // (crash vs graceful shutdown) stays distinguishable in
                    // logs and history.
                    if let Err(e) = crate::agent::session_lifecycle::on_exit_sweep() {
                        tracing::error!("Failed to mark sessions as suspended on exit: {}", e);
                    }
                    agent::process::kill_all_sessions();
                }
                tauri::RunEvent::Exit => {
                    tracing::info!("App exit complete");
                }
                tauri::RunEvent::WindowEvent { label, event, .. } => match event {
                    tauri::WindowEvent::CloseRequested { .. } => {
                        USER_CLOSE_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
                        diagnostics::mark_expected_exit(
                            diagnostics::ExpectedExitReason::CloseRequested,
                        );
                        tracing::info!(window = %label, "Window close requested by user");
                    }
                    tauri::WindowEvent::Destroyed => {
                        let user_initiated =
                            USER_CLOSE_REQUESTED.load(std::sync::atomic::Ordering::SeqCst);
                        if user_initiated {
                            tracing::info!(window = %label, "Window destroyed after user close request");
                        } else {
                            // Forensics for the silent-exit class of failures
                            // seen on 2026-08-26: NVIDIA driver resets killed
                            // WebView2 twice and Buildmesh quit without ever
                            // reaching ExitRequested, leaving no fingerprint.
                            // A Destroyed-without-CloseRequested is that
                            // failure mode surfacing through an observable
                            // seam. Log loudly, then bounce the hub back so
                            // agents keep running (guarded against loops).
                            tracing::error!(
                                window = %label,
                                "Window destroyed WITHOUT a user close request — \
                                 webview/GPU process death suspected"
                            );
                            #[cfg(target_os = "windows")]
                            tracing::info!(
                                "Deferring unexpected-exit recovery to the external watchdog"
                            );

                            #[cfg(not(target_os = "windows"))]
                            match _app_handle.path().app_data_dir() {
                                Ok(app_dir) => match diagnostics::relaunch_detached(
                                    &app_dir.join("logs"),
                                ) {
                                    Ok(true) => {
                                        tracing::info!("Auto-relaunch spawned (guard passed)");
                                    }
                                    Ok(false) => {
                                        tracing::warn!(
                                            "Auto-relaunch skipped — another auto-relaunch \
                                             fired less than {}s ago",
                                            diagnostics::AUTO_RELAUNCH_COOLDOWN_SECS
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!("Auto-relaunch failed: {}", e);
                                    }
                                },
                                Err(e) => {
                                    tracing::error!(
                                        "Auto-relaunch failed (no app dir): {}",
                                        e
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        });
}

/// Everything that touches the app-data profile, and only ever on behalf of
/// the process that owns it (issue #1521).
///
/// Called exclusively from inside the ownership gate in `setup`, so a losing
/// launch cannot reach `db::init`, the crash-recovery sweep, or any of the
/// background workers. Takes the identity rather than a raw path so the
/// canonicalised profile dir has exactly one owner and the claim target can be
/// logged with the rest of the startup line.
///
/// `bootstrap` is the [`startup::Bootstrap`] installed before this function
/// was entered, so the database, preferences, and migrations below are all
/// already inside a live `tracing` pipeline (issue #1525). That is the whole
/// reason the non-fatal `warn!` lines in this function have ever been
/// reviewable: before the bootstrap, they were emitted before a subscriber
/// existed and went nowhere.
///
/// A fatal failure here has already been shown to the user through
/// [`startup::report`], so the returned error only has to end the process.
fn run_profile_startup(
    app: &mut tauri::App,
    identity: &instance_guard::ProfileIdentity,
    bootstrap: &'static startup::Bootstrap,
) -> Result<(), Box<dyn std::error::Error>> {
    let app_dir = identity.profile_dir().to_path_buf();
    let db_path = app_dir.join("buildmesh.db");
    // State recovery (issue #1537), in the only order that is safe:
    //
    // 1. A restore staged by the user in the *previous* session is applied
    //    first, and specifically before `db::init` — at this point there is no
    //    connection, no reader pool, no circuit worker, and no PTY, so
    //    "close workers/connections safely" is satisfied structurally instead
    //    of by a shutdown race. The hook also drops the stale `-wal`/`-shm`
    //    sidecars, which SQLite would otherwise replay onto the restored file.
    // 2. A snapshot of the pre-upgrade state is taken when the database is
    //    about to be evolved, so an upgrade is always reversible.
    //
    // Both are non-fatal by design: a recovery failure must not stop the app
    // from starting, because the user needs the UI to reach Restore. Anything
    // they could not do is logged and surfaced in Settings > Data &
    // Diagnostics as a `RecoveryNotice`.
    match services::state_recovery::apply_pending_restore(&app_dir) {
        Ok(Some(applied)) => tracing::info!(
            source = %applied.source,
            ?applied.applied,
            "state recovery: applied a restore staged in a previous session"
        ),
        Ok(None) => {}
        Err(error) => tracing::error!(
            %error,
            "state recovery: could not apply a staged restore; continuing with current state"
        ),
    }
    match services::state_recovery::snapshot_before_migration(&app_dir, &db_path) {
        Ok(Some(snapshot)) => tracing::info!(
            path = %snapshot.path,
            from_version = snapshot.schema_version,
            kind = %snapshot.kind,
            "state recovery: preserved the pre-migration state"
        ),
        Ok(None) => {}
        Err(error) => tracing::error!(
            %error,
            "state recovery: could not snapshot the pre-migration state; \
             proceeding with the upgrade without a rollback point"
        ),
    }

    // Initialize database.
    //
    // The one failure that most often stops Buildmesh opening, and the reason
    // `buildmesh.log` now exists before it: a corrupt or unreadable database
    // used to return here as an opaque error with no log behind it. The
    // failure is typed, so a corrupt image is reported as damage with
    // instructions and an unopenable one as a permissions problem — and
    // neither ever touches the file.
    //
    // Runs *after* the recovery hooks above so that a user who staged a
    // restore is applying it to the database that then gets opened and
    // migrated, rather than migrating the one they are replacing.
    db::init(&db_path).map_err(|error| {
        startup::report(
            startup::StartupFailure::database(&db_path, &error)
                .with_paths(Some(bootstrap.main_log().to_path_buf()), Some(app_dir.clone())),
        )
    })?;

    // Wire the preferences module to the same on-disk location as the DB.
    // This MUST run before the v19 custom-account migration below —
    // `db::init`'s first-class migration block is preferences-independent
    // (it only rewrites the hardcoded 'minimax'/'kimi' ids), but the
    // custom-account block reads the user's stored `ProviderAccount`
    // list, which requires `APP_DATA_DIR` to be set. See
    // `db::migrate_agent_node_provider_id_custom_accounts` (issue #575).
    preferences::init(app_dir.clone());
    // A `preferences.json` that will not parse degrades to defaults by design
    // — the resolver accessors log and carry on — so this is deliberately NOT
    // a fatal `StartupStage`: `preferences::init` cannot fail, and refusing to
    // launch over a settings file would strand an app whose database is fine.
    // What it *is*, though, is a user who believes settings that are not in
    // effect are, so make it loud and name the file. This is the first read
    // of it in the session, and — unlike the resolver's own line — it lands in
    // `buildmesh.log` now that the bootstrap subscriber is installed.
    if let Err(error) = preferences::load() {
        tracing::error!(
            "startup stage=preferences: could not read {} — running with \
             default preferences for this session: {}",
            app_dir.join("preferences.json").display(),
            error
        );
    }
    for pairing in preferences::provider_pairings()
        .into_iter()
        .filter(|pairing| pairing.surface == preferences::ApiSurface::OpenAI)
    {
        commands::preferences::schedule_pairing_verification(
            app.handle().clone(),
            pairing.harness_id,
            pairing.provider_id,
        );
    }

    // v19 Spawn Option composite-id migration, custom-account block
    // (issue #575). The first-class block ('minimax'/'kimi') already
    // ran inside `db::init`; this call only handles user-stored
    // custom accounts (e.g. a user-typed "deepseek" account).
    // Idempotent — the underlying UPDATE has a `provider NOT LIKE
    // '%:%'` guard, so re-running on a v19+ DB is a no-op.
    if let Err(e) = db::ensure_agent_node_provider_id_custom_accounts_migrated(
        &db::write_conn(),
        &preferences::provider_accounts(),
    ) {
        tracing::warn!(
            "v19 custom-account migration failed (non-fatal, archived nodes \
             will keep legacy bare ids until the next launch): {}",
            e
        );
    }

    // Mesh-default Spawn Option composite-id safety net (v19 follow-up).
    // The v19 first-class block in `db::init` rewrote `agent_nodes.provider`
    // from bare → composite but never touched `meshes.default_provider` —
    // a pre-#575 mesh whose default was set to "minimax" or "kimi" kept
    // the legacy bare form after upgrade. Without this safety net, the
    // bare form routes through `resolve_provider_env` to the keyed
    // **account** instead of the post-#575 proxied pairing, silently
    // spawning Claude-CLI sessions against the wrong endpoint.
    // Idempotent — the `WHERE default_provider IN (...)` guard is a
    // no-op on already-migrated rows.
    if let Err(e) = db::ensure_mesh_default_provider_normalized(&db::write_conn()) {
        tracing::warn!(
            "mesh-default provider normalization failed (non-fatal, meshes \
             will keep legacy bare ids until the next launch): {}",
            e
        );
    }

    // App-wide default-provider Spawn Option composite-id safety net.
    // Companion to `ensure_mesh_default_provider_normalized` for the
    // `preferences.json::default_provider` field — the v19 migration
    // never rewrote that either. Without this, a user whose app-wide
    // default was set before #575 keeps the legacy bare form in
    // preferences.json, and `resolve_default_provider` returns it
    // verbatim to `+`-click spawns on meshes without a per-mesh
    // override. Idempotent — already-normalized values are a no-op.
    if let Err(e) = preferences::ensure_default_provider_normalized() {
        tracing::warn!(
            "app-wide default provider normalization failed (non-fatal, spawns \
             without a per-mesh override will keep the legacy bare id until \
             the next launch): {}",
            e
        );
    }

    // Auto-detect installed agent harnesses and populate dynamic profiles
    // Native PATH/config scan plus a bounded probe of the default WSL
    // distribution. Additive merge preserves existing profile identities.
    // Failure is non-fatal; legacy provider entries remain available.
    let scan_start = std::time::Instant::now();
    let detected = agent::detection::detect_installed_profiles();
    match preferences::merge_detected_profiles(detected) {
        Ok(added) => tracing::info!(
            "Harness detection: {} new profile(s) added in {:?}",
            added,
            scan_start.elapsed()
        ),
        Err(e) => tracing::warn!("Harness detection merge failed: {}", e),
    }

    if let Err(error) = services::agent_node::migrate_launch_history() {
        tracing::warn!("Launch history migration failed: {error}");
    }

    // Leave the bootstrap phase (issue #1525). The subscriber and its
    // size-bounded, fixed-name `buildmesh.log` writer were installed before
    // `db::init`; this only stops mirroring the log to stderr, so a long
    // `debug`-level session does not spray into a developer's terminal.
    //
    // There is deliberately no second `tracing_subscriber::fmt().init()` here.
    // That call is what used to sit at this point, which is precisely why the
    // database, preferences, and migrations above ran with no logging at all.
    // Installing once, early, means early records are already in the same
    // bounded log the rest of the session appends to — nothing to bridge, and
    // no "a global default subscriber has already been set" panic.
    bootstrap.promote();

    // The claim target is in the startup line on purpose (issue #1521): when
    // a user reports that Buildmesh "did not start", the first question is
    // which process owns the profile, and this is the answer.
    tracing::info!(
        "Buildmesh started — db at {:?}, profile claim {:?}",
        db_path,
        identity.claim_target()
    );

    let log_dir = bootstrap.log_dir().to_path_buf();

    if let Err(error) = diagnostics::start_crash_watchdog(&log_dir) {
        tracing::error!("failed to start external crash watchdog: {error}");
    }

    // Commit legacy cancellation before crash recovery can offer auto-resume.
    services::legacy_retirement::retire_legacy_automation().map_err(|error| {
        startup::report(
            startup::StartupFailure::services("retiring legacy automation", &error)
                .with_paths(Some(bootstrap.main_log().to_path_buf()), Some(app_dir.clone())),
        )
    })?;

    // Crash recovery: any sessions still marked 'running' from a previous
    // crash have no live process. Mark them suspended for auto-resume.
    // Lives inside SessionLifecycle (issue #132) — the lifecycle
    // module is the single owner of every state transition, including
    // the startup sweep.
    match crate::agent::session_lifecycle::recover_from_crash() {
        Ok(count) if count > 0 => {
            tracing::info!("Crash recovery: marked {} orphaned sessions as suspended", count);
        }
        Ok(_) => {}
        Err(e) => tracing::error!("Crash recovery failed: {}", e),
    }

    // Reconcile worktree removals that didn't finish before a previous
    // exit. A close records the intent durably, so a mid-cleanup quit is
    // resumed here rather than orphaning the directory forever (#243).
    commands::agent_node::drain_pending_removals(app.handle().clone());

    // Log window creation and set title with git commit
    let git_sha = env!("GIT_SHA");
    if let Some(window) = app.get_webview_window("main") {
        // The prefix is shared with `instance_guard`, which matches on it to
        // find this window when a second launch is forwarded here (issue #1521).
        let title = format!(
            "{}{}",
            instance_guard::MAIN_WINDOW_TITLE_PREFIX,
            git_sha
        );
        window.set_title(&title).ok();
        tracing::info!("Main window found, ready to load content: {}", title);
    } else {
        tracing::warn!("Main window not found during setup");
    }

    // Dev builds (identifier `*.dev`) run alongside the stable hub.
    // Offset every server port by 1000 so the two instances never
    // contend on 1991/1992. Derived from the bundle identifier so a
    // single config overlay flips binary, data dir, and ports together.
    let port_offset = http::port_offset(&app.config().identifier);

    crate::agent::provider::muse::telemetry::bind_app(app.handle().clone());

    // Start HTTP test server (1991, or 2991 for the dev profile) for Playwright E2E tests
    commands::test::start_test_server(app.handle().clone(), port_offset);

    // Start embedded HTTP/WebSocket server for mobile remote access
    http_server::start_http_server(app.handle().clone(), port_offset);

    // Pre-spawn Worktree Pool reconcile (issue #609, PRD #608). Runs
    // once per startup AFTER the HTTP server has bound, so a
    // Playwright run hitting `/api/...` immediately after launch
    // doesn't compete with the reconcile's per-mesh `git worktree add`
    // for the DB mutex (the reconcile would otherwise hold the mutex
    // across N+1 sequential round-trips while the HTTP server tries
    // to accept requests). Prunes stale rows, then ensures one warm
    // detached-HEAD worktree per worktree-enabled mesh. Best-effort —
    // failures are logged and the spawn path falls back to cold
    // checkout when the pool is empty.
    //
    // The handle is captured so `reconcile_on_startup` can emit
    // `pool-count-changed` at end-of-pass (settles the
    // 0 → target transition for any probe opened during boot).
    let reconcile_handle = app.handle().clone();
    std::thread::spawn(move || {
        services::warm_pool::reconcile_on_startup(reconcile_handle);
    });

    // Background pool maintenance worker (issue #613). A long-lived
    // thread that, once the app has been idle (no terminal output /
    // keypresses) for `IDLE_SILENCE`, tops every worktree-enabled
    // mesh's pool back up to its `pre_spawn_pool_size` target — so the
    // pool self-heals after spawns/closes without waiting for the next
    // claim or app restart. Debounced (never competes with active
    // agent I/O) and serialized behind the same fill lock as
    // `refill_after_claim`, so concurrent spawns can't trigger
    // overlapping `git worktree add` fills.
    //
    // The handle is captured so `drain_and_fill_for_mesh` (called per-mesh
    // by the worker, not the legacy `maintain_all_pools` aggregator)
    // can emit `pool-count-changed` from its inner drain/fill calls.
    services::pool_worker::start_background_worker(app.handle().clone());

    // Retry one-way retirement of historical legacy runs; never launch new agents.
    services::legacy_retirement::start_retirement_worker();

    // Autopilot Circuits worker (spec #1205 / walking skeleton
    // #1206). Dedicated OS thread with a fast tick + condvar
    // wake; drives the pure circuit stepper's decisions through
    // the effect executor (spawn agent nodes, PTY injection,
    // node status, notifications).
    services::circuit_worker::start_circuit_worker(app.handle().clone());

    // Coordinator drive ledger GC (issue #750, item 3). One prune
    // pass every 30 minutes keeps the `coordinator_drive_prompts`
    // table bounded by the 7-day retention window so the ledger's
    // size stays proportional to "unique drives per week" rather
    // than "unique drives ever". Independent of the autopilot
    // worker because the prune isn't mesh-scoped (it's a single
    // bounded DELETE on `created_at`).
    services::coordinator_ledger_maintenance::start_worker();

    // Always-on resource diagnostics (issue: background-refresh grind).
    // A low-frequency sampler writes process vitals (memory, handles,
    // threads, live child processes) + per-subsystem counters to a
    // dedicated, size-bounded `logs/diagnostics.log` the user can hand
    // back after a session degrades. Off the hot path; opt out with
    // `BUILDMESH_DIAG=0`, retune with `BUILDMESH_DIAG_INTERVAL_MS`.
    diagnostics::start_sampler(log_dir.clone());

    // Install panic hook that logs thread ID + backtrace on every panic
    let crash_log_path = app_dir.join("logs").join("panic.log");
    std::panic::set_hook(Box::new(move |info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic".to_string()
        };
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_else(|| "unknown".to_string());
        let thread_info = std::thread::current();
        let thread = thread_info.name().unwrap_or("unnamed");
        let thread_id = thread_info.id();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let backtrace = std::backtrace::Backtrace::capture();
        let panic_msg = format!(
            "[{}] PANIC in thread '{}' ({:?}): {} at {}\nBacktrace:\n{}",
            timestamp, thread, thread_id, msg, location, backtrace
        );
        eprintln!("{}", panic_msg);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_log_path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{}", panic_msg);
            // panic = "abort" kills the process via __fastfail; the OS
            // file buffer would otherwise discard this write.
            let _ = file.flush();
            let _ = file.sync_all();
        }
    }));

    Ok(())
}

/// Report a profile-ownership failure and produce the fatal startup error.
///
/// Issue #1521 requires that failing to establish ownership is a *visible*
/// startup error rather than permission to carry on: an app that cannot tell
/// which profile it owns must not open that profile's database, because the
/// next process to do the same would sweep the live instance's Agent Nodes.
///
/// The surface is the one in [`startup`], so an ownership failure reads like
/// every other fatal startup failure — same native dialog, same log, same
/// "copy details" — instead of the second, subtly different message box this
/// used to raise. The profile's `logs/profile-ownership.log` line is kept:
/// that file exists even when the main log never opened, and it is what the
/// existing troubleshooting page points at.
fn ownership_failure(
    identifier: &str,
    app_dir: &std::path::Path,
    error: instance_guard::OwnershipError,
) -> Box<dyn std::error::Error> {
    if let Err(e) = instance_guard::record_claim_failure(app_dir, identifier, &error) {
        eprintln!("could not record the ownership failure: {e}");
    }
    let failure = startup::StartupFailure::profile_ownership(&error, app_dir, identifier);
    // `with_paths` fills in the log location the bootstrap resolved, so the
    // dialog can offer "open log folder" for this failure too.
    let failure = match startup::installed_paths() {
        Some(paths) => failure.with_paths(Some(paths.main_log), Some(paths.profile_dir)),
        None => failure,
    };
    startup::report(failure)
}

/// Set when the user (or frontend) closes a window through the normal
/// `CloseRequested` path. A later `Destroyed` for that window is expected;
/// a `Destroyed` without this flag means something external killed the
/// window's webview — the silent-exit signature.
static USER_CLOSE_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Retract a user close request the frontend vetoed (issue #1501).
///
/// The `CloseRequested` handler marks the exit expected eagerly, but the
/// frontend exit-confirmation modal can still cancel the close ("Keep
/// Working"). Without this reset, the stale flag + supervisor marker file
/// would make a later real crash (e.g. a GPU-driver WebView2 kill) look
/// like an expected exit and suppress the watchdog auto-relaunch. Called
/// by the `cancel_window_close` command.
pub(crate) fn cancel_close_request() {
    USER_CLOSE_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
    diagnostics::clear_expected_exit();
}

/// Mark a user-initiated teardown that bypasses `CloseRequested`
/// (issue #1501).
///
/// The `exit_application` command never passes through
/// `WindowEvent::CloseRequested`, so without this the `Destroyed` handler
/// would classify the confirmed exit as the webview/GPU-death crash
/// signature and (on non-Windows) fire the crash auto-relaunch.
pub(crate) fn mark_user_close_requested() {
    USER_CLOSE_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod close_cancel_tests {
    use super::cancel_close_request;
    use super::USER_CLOSE_REQUESTED;

    /// Cancelling resets the flag so a later `Destroyed`-without-close is
    /// still classified as a crash (issue #1501: the "Keep Working" path
    /// must restore the watchdog invariant).
    #[test]
    fn cancel_close_request_resets_flag() {
        USER_CLOSE_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
        cancel_close_request();
        assert!(!USER_CLOSE_REQUESTED.load(std::sync::atomic::Ordering::SeqCst));
    }
}
