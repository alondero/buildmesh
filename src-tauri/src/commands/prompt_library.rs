use crate::preferences::{self, model::SavedPrompt, prompt_library, PreferencesStore};

fn list_saved_prompts_blocking(store: &PreferencesStore) -> Result<Vec<SavedPrompt>, String> {
    Ok(store.load()?.saved_prompts)
}

#[tauri::command]
pub async fn list_saved_prompts() -> Result<Vec<SavedPrompt>, String> {
    super::run_blocking("list_saved_prompts", || {
        list_saved_prompts_blocking(&preferences::ambient_store()?)
    })
    .await
}

#[tauri::command]
pub async fn save_prompt(
    id: Option<String>,
    name: String,
    body: String,
) -> Result<SavedPrompt, String> {
    super::run_blocking("save_prompt", move || {
        prompt_library::save(&preferences::ambient_store()?, id, name, body)
    })
    .await
}

#[tauri::command]
pub async fn delete_saved_prompt(id: String) -> Result<(), String> {
    super::run_blocking("delete_saved_prompt", move || {
        prompt_library::delete(&preferences::ambient_store()?, &id)
    })
    .await
}
