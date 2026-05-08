use tauri::command;

#[command]
pub fn submit_terminal_snapshot(request_id: String, data: String) {
    crate::http_server::fulfill_snapshot(&request_id, data);
}
