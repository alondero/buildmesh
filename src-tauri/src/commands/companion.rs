use crate::companion::{CompanionSettingsUpdate, CompanionState, CompanionTask};
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
pub async fn companion_execute_prompt(
    app: AppHandle,
    state: State<'_, CompanionState>,
    prompt: String,
) -> Result<CompanionTask, String> {
    let (snapshot, cancel) = state.start(prompt)?;
    let owned = state.inner().clone();
    let task = snapshot.clone();
    tauri::async_runtime::spawn(async move {
        let result = crate::companion::runner::run(&app, &owned, &task, cancel.clone()).await;
        let cancelled = *cancel.borrow();
        let event = if result.is_ok() || cancelled {
            "companion://completed"
        } else {
            "companion://error"
        };
        owned.update(&app, &task.id, event, |t| {
            t.confirmation = None;
            if cancelled {
                t.status = "cancelled".into();
            } else {
                match result {
                    Ok(response) => {
                        t.status = "completed".into();
                        t.response = Some(response);
                    }
                    Err(error) => {
                        t.status = "error".into();
                        t.error = Some(error);
                    }
                }
            }
        });
    });
    Ok(snapshot)
}

#[tauri::command]
pub fn companion_get_active_task(state: State<'_, CompanionState>) -> Option<CompanionTask> {
    state.snapshot()
}

#[tauri::command]
pub fn companion_cancel_task(
    app: AppHandle,
    state: State<'_, CompanionState>,
    task_id: String,
) -> Result<(), String> {
    let snapshot = state.cancel(&task_id)?;
    let _ = app.emit("companion://step", snapshot);
    Ok(())
}

#[tauri::command]
pub fn companion_respond_confirmation(
    state: State<'_, CompanionState>,
    task_id: String,
    revision: u32,
    approved: bool,
) -> Result<(), String> {
    state.respond(&task_id, revision, approved)
}

#[tauri::command]
pub async fn companion_save_settings(settings: CompanionSettingsUpdate) -> Result<(), String> {
    super::run_blocking("companion_save_settings", move || {
        settings.validate()?;
        crate::preferences::update(|p| settings.apply(&mut p.companion))?;
        Ok(())
    })
    .await
}
