//! Conductor Clone — Rust Backend
//! AI Agent Orchestration Hub for Windows + WSL

mod commands;
mod db;
mod env;
mod models;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info")
    ).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Initialize database
            let app_dir = app.path().app_data_dir().unwrap();
            std::fs::create_dir_all(&app_dir)?;
            let db_path = app_dir.join("conductor.db");
            db::init(&db_path)?;

            log::info!("Conductor Clone started — db at {:?}", db_path);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Workspace
            commands::workspace::create_workspace,
            commands::workspace::list_workspaces,
            commands::workspace::list_workspaces_by_project,
            commands::workspace::get_workspace,
            commands::workspace::archive_workspace,
            commands::workspace::restore_workspace,
            commands::workspace::update_workspace_status,
            // Project
            commands::project::add_project,
            commands::project::create_project,
            commands::project::list_projects,
            commands::project::delete_project,
            // Agent
            commands::agent::spawn_agent,
            commands::agent::kill_agent,
            commands::agent::is_agent_running,
            // Checkpoint
            commands::checkpoint::create_checkpoint,
            commands::checkpoint::list_checkpoints,
            commands::checkpoint::revert_to_checkpoint,
            commands::checkpoint::diff_checkpoints,
            // Diff
            commands::diff::diff_files,
            commands::diff::diff_workspace_checkpoint,
            // Terminal
            commands::terminal::spawn_pty,
            commands::terminal::write_pty,
            commands::terminal::close_pty,
            commands::terminal::spawn_shell,
            // File watcher
            commands::file_watcher::watch_workspace,
            commands::file_watcher::unwatch_workspace,
            // MCP
            commands::mcp::list_mcp_servers,
            // PR
            commands::pr::create_pr,
            commands::pr::merge_pr,
            commands::pr::get_current_branch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
