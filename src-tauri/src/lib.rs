//! Buildmesh — Rust Backend
//! AI Agent Orchestration Hub for Windows + WSL

mod commands;
mod db;
mod env;
mod models;

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

            // Log window creation
            if let Some(_window) = app.get_webview_window("main") {
                tracing::info!("Main window found, ready to load content");
            } else {
                tracing::warn!("Main window not found during setup");
            }

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
            // Project
            commands::project::add_project,
            commands::project::create_project,
            commands::project::list_projects,
            commands::project::delete_project,
            // Agent
            commands::agent::spawn_agent,
            commands::agent::kill_agent,
            commands::agent::is_agent_running,
            commands::agent::debug_list_agents,
            commands::agent::send_to_agent,
            commands::agent::write_to_agent,
            // Checkpoint
            commands::checkpoint::create_checkpoint,
            commands::checkpoint::list_checkpoints,
            commands::checkpoint::revert_to_checkpoint,
            commands::checkpoint::diff_checkpoints,
            // Diff
            commands::diff::diff_files,
            commands::diff::diff_session_checkpoint,
            // File tree
            commands::file_tree::list_directory,
            // Git
            commands::git::get_git_status,
            // Terminal
            commands::terminal::spawn_pty,
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
