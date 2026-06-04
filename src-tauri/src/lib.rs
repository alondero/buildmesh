//! Buildmesh — Rust Backend
//! AI Agent Orchestration Hub for Windows + WSL

pub mod agent;
mod commands;
mod db;
mod env;
mod http;
mod http_server;
pub mod models;
mod preferences;
pub mod process_util;
mod pty;
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
            preferences::init(app_dir.clone());

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
            // Agent Node
            commands::agent_node::create_session,
            commands::agent_node::list_sessions,
            commands::agent_node::list_sessions_by_project,
            commands::agent_node::list_all_sessions,
            commands::agent_node::get_session,
            commands::agent_node::archive_session,
            commands::agent_node::restore_session,
            commands::agent_node::update_session_status,
            commands::agent_node::delete_session,
            commands::agent_node::get_worktree_close_safety,
            commands::agent_node::set_cli_session_id,
            commands::agent_node::set_active_session,
            commands::agent_node::rename_session,
            // Mesh
            commands::mesh::add_project,
            commands::mesh::create_project,
            commands::mesh::create_test_project,
            commands::mesh::list_projects,
            commands::mesh::delete_project,
            commands::mesh::update_project_layout,
            commands::mesh::update_project_positions,
            commands::mesh::get_root_token,
            commands::mesh::get_local_ip,
            commands::mesh::get_default_provider,
            // App preferences (buildmesh-wide)
            commands::preferences::get_app_preferences,
            commands::preferences::set_app_default_provider,
            // Agent
            commands::agent::spawn_agent,
            commands::agent::resize_agent,
            commands::agent::kill_agent,
            commands::agent::is_agent_running,
            commands::agent::debug_list_agents,
            commands::agent::send_to_agent,
            commands::agent::write_to_agent,
            commands::agent::auto_resume_sessions,
            commands::agent::debug_crash_snapshot,
            commands::agent::list_providers,
            commands::agent::spawn_issue_agent,
            commands::agent::spawn_handover_agent,
            // Checkpoint
            commands::checkpoint::create_checkpoint,
            // Build/Run
            commands::build_run::build_run,
            commands::build_run::get_mesh_config,
            commands::build_run::close_build_run,
            commands::build_run::ensure_mesh_config,
            // Mesh config
            commands::mesh_config::get_mesh_properties,
            commands::mesh_config::update_mesh_name,
            commands::mesh_config::update_mesh_field,
            commands::mesh_config::update_worktree_base_ref,
            commands::mesh_config::remove_worktree_base_ref,
            commands::mesh_config::update_mesh_use_worktree,
            // Project detection (presets)
            commands::project_detect::detect_mesh_project,
            commands::checkpoint::list_checkpoints,
            commands::checkpoint::revert_to_checkpoint,
            commands::checkpoint::diff_checkpoints,
            // Diff
            commands::diff::diff_files,
            commands::diff::diff_session_checkpoint,
            commands::diff::diff_file_against_head,
            // File tree
            commands::file_tree::list_directory,
            commands::file_tree::open_in_editor,
            commands::file_tree::get_user_config_dir,
            commands::file_tree::to_host_path,
            // Git
            commands::git::get_git_status,
            commands::git::get_git_branch_status,
            commands::git::get_git_summary,
            commands::git::check_is_git_repo,
            commands::git::get_default_branch,
            commands::git::git_sync,
            // Prune (branches & worktrees)
            commands::prune::get_git_prune_info,
            commands::prune::delete_branches,
            commands::prune::delete_worktrees,
            commands::prune::prune_remote_tracking,
            // Terminal
            commands::terminal::spawn_pty,
            commands::terminal::resize_pty,
            commands::terminal::write_pty,
            commands::terminal::close_pty,
            commands::terminal::spawn_shell,
            // File watcher
            commands::file_watcher::watch_session,
            commands::file_watcher::unwatch_session,
            // Clipboard (native read bypasses macOS WKWebView permission popup)
            commands::clipboard::read_clipboard,
            // Frontend log bridge
            commands::frontend_log::log_frontend,
            // Attention
            commands::attention::register_attention_session,
            commands::attention::clear_attention_session,
            commands::attention::is_attention_pending,
            // PR
            commands::pr::create_pr,
            commands::pr::merge_pr,
            commands::pr::get_current_branch,
            commands::pr::check_gh_auth,
            commands::pr::create_pr_for_mesh,
            commands::pr::get_repo_issues,
            // AI context portability
            commands::ai_context::detect_ai_context,
            commands::ai_context::create_ai_context_portability_pr,
            // Remote
            commands::remote::submit_terminal_snapshot,
            // Session Discovery
            commands::session_discovery::discover_sessions,
            commands::session_discovery::import_discovered_session,
            // Usage
            commands::usage::get_provider_usage,
            commands::usage::get_all_provider_usage,
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
