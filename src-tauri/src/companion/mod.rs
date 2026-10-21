//! Single-intent fleet coordination. The task outlives its presentation surface.
pub mod runner;
pub mod tools;

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "CompanionSettings.ts")]
pub struct CompanionSettings {
    pub launch_configuration: Option<String>,
    pub spawn_limit: u32,
}

impl Default for CompanionSettings {
    fn default() -> Self {
        Self {
            launch_configuration: None,
            spawn_limit: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "CompanionTask.ts")]
pub struct CompanionTask {
    pub id: String,
    pub revision: u32,
    pub generation: u32,
    pub prompt: String,
    pub status: String,
    pub steps: Vec<String>,
    pub confirmation: Option<String>,
    pub response: Option<String>,
    pub error: Option<String>,
    pub navigation: Option<CompanionNavigation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "CompanionNavigation.ts")]
pub struct CompanionNavigation {
    pub sequence: u32,
    #[ts(as = "Option<i32>")]
    pub node_id: Option<i64>,
    #[ts(as = "Option<i32>")]
    pub mesh_id: Option<i64>,
}

struct ActiveTask {
    snapshot: CompanionTask,
    cancel: watch::Sender<bool>,
    confirmation: Option<tokio::sync::oneshot::Sender<bool>>,
}

#[derive(Clone, Default)]
pub struct CompanionState(Arc<Mutex<Option<ActiveTask>>>);

impl CompanionState {
    pub fn snapshot(&self) -> Option<CompanionTask> {
        self.0.lock().unwrap().as_ref().map(|t| t.snapshot.clone())
    }

    pub fn start(&self, prompt: String) -> Result<(CompanionTask, watch::Receiver<bool>), String> {
        if prompt.trim().is_empty() || prompt.len() > 16_000 {
            return Err("Enter a prompt between 1 and 16000 bytes".into());
        }
        let mut slot = self.0.lock().unwrap();
        if slot.as_ref().is_some_and(|t| {
            matches!(
                t.snapshot.status.as_str(),
                "running" | "confirming" | "cancelling"
            )
        }) {
            return Err("A Companion task is already active".into());
        }
        let (cancel, receiver) = watch::channel(false);
        let snapshot = CompanionTask {
            id: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            generation: slot.as_ref().map_or(1, |t| t.snapshot.generation + 1),
            prompt,
            status: "running".into(),
            steps: vec![],
            confirmation: None,
            response: None,
            error: None,
            navigation: None,
        };
        *slot = Some(ActiveTask {
            snapshot: snapshot.clone(),
            cancel,
            confirmation: None,
        });
        Ok((snapshot, receiver))
    }

    pub fn update(
        &self,
        app: &AppHandle,
        id: &str,
        event: &str,
        change: impl FnOnce(&mut CompanionTask),
    ) {
        let snapshot = {
            let mut slot = self.0.lock().unwrap();
            let Some(task) = slot.as_mut().filter(|t| t.snapshot.id == id) else {
                return;
            };
            change(&mut task.snapshot);
            task.snapshot.revision += 1;
            task.snapshot.clone()
        };
        let _ = app.emit(event, snapshot);
    }

    pub fn cancel(&self, id: &str) -> Result<CompanionTask, String> {
        let mut slot = self.0.lock().unwrap();
        let task = slot
            .as_mut()
            .filter(|t| t.snapshot.id == id)
            .ok_or("Task no longer exists")?;
        if !matches!(
            task.snapshot.status.as_str(),
            "running" | "confirming" | "cancelling"
        ) {
            return Err("Task has already finished".into());
        }
        task.cancel.send_replace(true);
        task.confirmation.take();
        task.snapshot.status = "cancelling".into();
        task.snapshot.confirmation = None;
        task.snapshot.revision += 1;
        Ok(task.snapshot.clone())
    }

    pub fn respond(&self, id: &str, revision: u32, approved: bool) -> Result<(), String> {
        let mut slot = self.0.lock().unwrap();
        let task = slot
            .as_mut()
            .filter(|t| t.snapshot.id == id && t.snapshot.revision == revision)
            .ok_or("Confirmation is no longer current")?;
        task.confirmation
            .take()
            .ok_or("No confirmation is pending")?
            .send(approved)
            .map_err(|_| "Confirmation expired".into())
    }

    pub async fn confirm(
        &self,
        app: &AppHandle,
        id: &str,
        description: String,
    ) -> Result<(), String> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let snapshot = {
            let mut slot = self.0.lock().unwrap();
            let task = slot
                .as_mut()
                .filter(|t| t.snapshot.id == id)
                .ok_or("Task no longer exists")?;
            if *task.cancel.borrow() {
                return Err("Task cancelled".into());
            }
            task.confirmation = Some(sender);
            task.snapshot.status = "confirming".into();
            task.snapshot.confirmation = Some(description);
            task.snapshot.revision += 1;
            task.snapshot.clone()
        };
        let _ = app.emit("companion://confirmation-needed", snapshot);
        let approved = receiver.await.unwrap_or(false);
        self.update(app, id, "companion://step", |t| {
            if t.status != "cancelling" {
                t.status = "running".into();
            }
            t.confirmation = None;
        });
        if approved {
            Ok(())
        } else {
            Err("Action cancelled by user".into())
        }
    }
}

#[cfg(test)]
mod tests;
