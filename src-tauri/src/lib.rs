//! Buildmesh — Rust Backend
//! AI Agent Orchestration Hub for Windows + WSL

pub mod agent;
pub mod autopilot;
mod commands;
mod coordinator;
mod db;
mod env;
mod git;
mod http;
mod http_server;
pub mod models;
mod node_turn;
mod preferences;
pub mod process_util;
mod pty;
pub mod sandbox;
pub mod secret_scrubber;
mod services;
mod session_capture;
mod session_naming;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            // Initialize database
            let app_dir = app.path().app_data_dir().unwrap();
            std::fs::create_dir_all(&app_dir)?;
            let db_path = app_dir.join("buildmesh.db");
            db::init(&db_path)?;

            // Wire the preferences module to the same on-disk location as the DB.
            // This MUST run before the v19 custom-account migration below —
            // `db::init`'s first-class migration block is preferences-independent
            // (it only rewrites the hardcoded 'minimax'/'kimi' ids), but the
            // custom-account block reads the user's stored `ProviderAccount`
            // list, which requires `APP_DATA_DIR` to be set. See
            // `db::migrate_agent_node_provider_id_custom_accounts` (issue #575).
            preferences::init(app_dir.clone());

            // v19 Spawn Option composite-id migration, custom-account block
            // (issue #575). The first-class block ('minimax'/'kimi') already
            // ran inside `db::init`; this call only handles user-stored
            // custom accounts (e.g. a user-typed "deepseek" account).
            // Idempotent — the underlying UPDATE has a `provider NOT LIKE
            // '%:%'` guard, so re-running on a v19+ DB is a no-op.
            if let Ok(conn) = db::get().lock() {
                if let Err(e) = db::ensure_agent_node_provider_id_custom_accounts_migrated(
                    &conn,
                    &preferences::provider_accounts(),
                ) {
                    tracing::warn!(
                        "v19 custom-account migration failed (non-fatal, archived nodes \
                         will keep legacy bare ids until the next launch): {}",
                        e
                    );
                }
            }

            // Auto-detect installed agent harnesses and populate dynamic profiles
            // (PRD #534 / issue #536). A dep-free in-process PATH scan — a few
            // hundred cached stat() calls, typically a couple of ms — so it runs
            // inline here. Additive merge: only newly-found tools are added, so
            // it's safe to re-run on every launch. Failure is non-fatal (the
            // legacy provider list still works), so we log and continue.
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

            // Set up file-based logging with tracing
            let log_dir = app_dir.join("logs");
            std::fs::create_dir_all(&log_dir)?;
            let file_appender = tracing_appender::rolling::never(&log_dir, "buildmesh.log");
            let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
            tracing_subscriber::fmt()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("buildmesh_lib=debug".parse().unwrap())
                    .add_directive("buildmesh=debug".parse().unwrap())
                    .add_directive("info".parse().unwrap()))
                .init();

            // Keep guard alive for app lifetime
            Box::leak(Box::new(_guard));

            tracing::info!("Buildmesh started — db at {:?}", db_path);

            // Crash recovery: any sessions still marked 'running' from a previous
            // crash have no live process. Mark them suspended for auto-resume.
            match db::mark_running_nodes_suspended() {
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
                let title = format!("Buildmesh - {}", git_sha);
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
            // The handle is captured so `maintain_all_pools` can emit
            // `pool-count-changed` from its inner drain/fill calls.
            services::pool_worker::start_background_worker(app.handle().clone());

            // Install panic hook that logs thread ID + backtrace on every panic
            let app_dir = app.path().app_data_dir().unwrap();
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
                }
            }));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Agent Node (issue #490: renamed from `*_session` to `*_agent_node`).
            commands::agent_node::create_agent_node,
            commands::agent_node::list_agent_nodes,
            commands::agent_node::get_agent_node,
            commands::agent_node::delete_agent_node,
            commands::agent_node::get_worktree_close_safety,
            commands::agent_node::rename_agent_node,
            commands::agent_node::update_agent_node_positions,
            // Mesh (renamed from `*_project` to `*_mesh`).
            commands::mesh::add_mesh,
            commands::mesh::create_mesh,
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
            commands::preferences::set_app_default_provider,
            commands::preferences::set_harness_order,
            commands::preferences::get_provider_accounts,
            commands::preferences::upsert_provider_account,
            commands::preferences::remove_provider_account,
            commands::preferences::get_provider_pairings,
            commands::preferences::compatible_providers_for_harness,
            commands::preferences::attach_proxied_provider,
            commands::preferences::remove_provider_pairing,
            // Coordinator read API (ADR-0008)
            commands::coordinator::get_coordinator_status,
            commands::coordinator::set_coordinator_api_enabled,
            commands::coordinator::generate_coordinator_read_token,
            commands::coordinator::set_coordinator_drive_enabled,
            commands::coordinator::generate_coordinator_drive_token,
            // Authorized devices (issue #502)
            commands::devices::list_device_sessions,
            commands::devices::revoke_device_session,
            // Network exposure (issue #501)
            commands::network::get_network_status,
            commands::network::set_lan_exposure_enabled,
            // Agent
            commands::agent::spawn_agent,
            commands::agent::resize_agent,
            commands::agent::kill_agent,
            commands::agent::is_agent_running,
            commands::agent::debug_list_agents,
            commands::agent::send_to_agent,
            commands::agent::write_to_agent,
            commands::agent::auto_resume_agent_nodes,
            commands::agent::debug_crash_snapshot,
            commands::agent::list_providers,
            commands::agent::spawn_issue_agent,
            commands::agent::spawn_handover_agent,
            commands::agent::create_issue_node,
            commands::agent::start_node_background,
            // Build/Run
            commands::build_run::build_run,
            commands::build_run::get_mesh_row,
            commands::build_run::close_build_run,
            commands::build_run::write_to_build_run,
            commands::build_run::resize_build_run,
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
            // Frontend log bridge
            commands::frontend_log::log_frontend,
            // Attention (renamed from `*_attention_session` to `*_attention_node`)
            commands::attention::register_attention_node,
            commands::attention::clear_attention_node,
            commands::attention::is_attention_pending,
            // PR
            commands::pr::create_pr,
            commands::pr::merge_pr,
            commands::pr::get_current_branch,
            commands::pr::check_gh_auth,
            commands::pr::create_pr_for_mesh,
            commands::agent::create_pr_node,
            commands::pr::get_repo_issues,
            commands::pr::get_open_pr_for_node,
            commands::pr::get_repo_pulls,
            commands::pr::get_pr_mergeability,
            commands::pr::get_prs_mergeability,
            commands::pr::get_pr_files,
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
            commands::usage::set_minimax_api_key,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = &event {
                tracing::info!("App exit requested, marking running sessions as suspended");
                if let Err(e) = db::mark_running_nodes_suspended() {
                    tracing::error!("Failed to mark sessions as suspended on exit: {}", e);
                }
                commands::agent::kill_all_agents();
            }
        });
}
