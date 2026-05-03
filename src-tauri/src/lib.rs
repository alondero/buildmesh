//! Buildmesh — Rust Backend
//! AI Agent Orchestration Hub for Windows + WSL

mod commands;
mod db;
mod env;
mod models;
mod naming;
mod session_namer;
mod turn_detector;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Initialize database
            let app_dir = app.path().app_data_dir().unwrap();
            std::fs::create_dir_all(&app_dir)?;
            let db_path = app_dir.join("buildmesh.db");
            db::init(&db_path)?;

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
            match db::mark_running_sessions_suspended() {
                Ok(count) if count > 0 => {
                    tracing::info!("Crash recovery: marked {} orphaned sessions as suspended", count);
                }
                Ok(_) => {}
                Err(e) => tracing::error!("Crash recovery failed: {}", e),
            }

            // Log window creation
            if let Some(_window) = app.get_webview_window("main") {
                tracing::info!("Main window found, ready to load content");
            } else {
                tracing::warn!("Main window not found during setup");
            }

            // Start HTTP test server on port 1991 for Playwright E2E tests
            commands::test::start_test_server(app.handle().clone());

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
            // Session
            commands::session::create_session,
            commands::session::list_sessions,
            commands::session::list_sessions_by_project,
            commands::session::get_session,
            commands::session::archive_session,
            commands::session::restore_session,
            commands::session::update_session_status,
            commands::session::delete_session,
            commands::session::set_active_session,
            // Project
            commands::project::add_project,
            commands::project::create_project,
            commands::project::create_test_project,
            commands::project::list_projects,
            commands::project::delete_project,
            commands::project::update_project_positions,
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
            // Checkpoint
            commands::checkpoint::create_checkpoint,
            commands::checkpoint::list_checkpoints,
            commands::checkpoint::revert_to_checkpoint,
            commands::checkpoint::diff_checkpoints,
            // Diff
            commands::diff::diff_files,
            commands::diff::diff_session_checkpoint,
            commands::diff::diff_file_against_head,
            // File tree
            commands::file_tree::list_directory,
            // Git
            commands::git::get_git_status,
            // Terminal
            commands::terminal::spawn_pty,
            commands::terminal::resize_pty,
            commands::terminal::write_pty,
            commands::terminal::close_pty,
            commands::terminal::spawn_shell,
            // File watcher
            commands::file_watcher::watch_session,
            commands::file_watcher::unwatch_session,
            // MCP
            commands::mcp::list_mcp_servers,
            // Attention
            commands::attention::register_attention_session,
            commands::attention::clear_attention_session,
            commands::attention::is_attention_pending,
            // PR
            commands::pr::create_pr,
            commands::pr::merge_pr,
            commands::pr::get_current_branch,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = &event {
                tracing::info!("App exit requested, marking running sessions as suspended");
                if let Err(e) = db::mark_running_sessions_suspended() {
                    tracing::error!("Failed to mark sessions as suspended on exit: {}", e);
                }
                commands::agent::kill_all_agents();
            }
        });
}
