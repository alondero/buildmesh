//! Profile-owned prompt mutations serialize with all other preference writes.
use super::{model::SavedPrompt, PreferencesStore};

pub fn save(
    store: &PreferencesStore,
    id: Option<String>,
    name: String,
    body: String,
) -> Result<SavedPrompt, String> {
    let name = name.trim().to_string();
    let body = body.trim().to_string();
    if name.is_empty() || body.is_empty() {
        return Err("Give the prompt a name and some text".into());
    }
    if name.chars().count() > 120 || body.len() > 64 * 1024 {
        return Err(
            "Use a name of at most 120 characters and prompt text of at most 64 KiB".into(),
        );
    }
    let creating = id.is_none();
    let prompt = SavedPrompt {
        id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name,
        body,
    };
    store.try_update(|prefs| {
        if creating {
            prefs.saved_prompts.push(prompt.clone());
        } else {
            let existing = prefs
                .saved_prompts
                .iter_mut()
                .find(|p| p.id == prompt.id)
                .ok_or("This prompt no longer exists. Save a new copy instead")?;
            *existing = prompt.clone();
        }
        Ok(())
    })?;
    Ok(prompt)
}

pub fn delete(store: &PreferencesStore, id: &str) -> Result<(), String> {
    store.try_update(|prefs| {
        let index = prefs
            .saved_prompts
            .iter()
            .position(|p| p.id == id)
            .ok_or("This prompt no longer exists")?;
        prefs.saved_prompts.remove(index);
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;
