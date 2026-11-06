//! One explicit custom-provider setup, committed as one preferences transaction.

use super::model::BillingMode;
use super::resolver::catalog::BUILTIN_PROVIDER_ACCOUNTS;
use super::spawn_configurations::{self, SpawnConfiguration};
use super::{ApiSurface, ModelTiers, PreferencesStore, ProviderAccount, ProviderPairing};

pub fn create_from(
    store: &PreferencesStore,
    name: String,
    api_key: String,
    surface: ApiSurface,
    base_url: String,
    model: String,
) -> Result<SpawnConfiguration, String> {
    let name = name.trim();
    let id = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase();
    if id.is_empty() {
        return Err("Custom provider needs a name containing letters or numbers".into());
    }
    if BUILTIN_PROVIDER_ACCOUNTS.iter().any(|a| a.id == id) {
        return Err("This provider is built in; select it from Add provider instead".into());
    }
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("API key is required".into());
    }
    let model = model.trim();
    if model.is_empty() {
        return Err("Model ID is required".into());
    }
    let base_url = validate_base_url(&base_url)?;
    let harness_id = match surface {
        ApiSurface::OpenAI => "codex",
        ApiSurface::Anthropic => "claude",
    };
    let pairing = ProviderPairing {
        harness_id: harness_id.into(),
        provider_id: id.clone(),
        surface,
        base_url: Some(base_url),
        model_tiers: ModelTiers {
            default: Some(model.into()),
            // One model works at every Claude tier until the user chooses to
            // specialize them in Advanced Provider Routes.
            sonnet: (surface == ApiSurface::Anthropic).then(|| model.into()),
            opus: (surface == ApiSurface::Anthropic).then(|| model.into()),
            fable: (surface == ApiSurface::Anthropic).then(|| model.into()),
            haiku: (surface == ApiSurface::Anthropic).then(|| model.into()),
            small_fast: (surface == ApiSurface::Anthropic).then(|| model.into()),
        },
    };
    let decision = super::pairing_compatibility(&pairing);
    if !decision.compatible {
        return Err(decision
            .reason
            .unwrap_or_else(|| "Incompatible provider route".into()));
    }
    let configuration = spawn_configurations::validate(SpawnConfiguration {
        id: format!("launch/{}", uuid::Uuid::new_v4()),
        name: name.into(),
        spawn_option_id: format!("{harness_id}:{id}"),
        model: Some(model.into()),
        ..Default::default()
    })?;
    store.try_update(|prefs| {
        if prefs.provider_accounts.iter().any(|a| a.id == id) {
            return Err(format!("A provider with id \"{id}\" already exists"));
        }
        prefs.provider_accounts.push(ProviderAccount {
            id: id.clone(),
            name: name.into(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some(api_key.into()),
        });
        // Removing an account deliberately retains its routes and recipes.
        // Re-adding the credential restores those; replace this explicitly
        // configured route rather than refusing an otherwise hidden orphan.
        super::upsert_provider_pairing(prefs, pairing.clone());
        prefs.spawn_configurations.push(configuration.clone());
        super::launch_configurations::resolve_for_edit(prefs, &configuration.id)?;
        Ok(())
    })?;
    Ok(configuration)
}

fn validate_base_url(value: &str) -> Result<String, String> {
    let value = value.trim().trim_end_matches('/');
    let url = super::resolver::pairing_compat::validate_endpoint_url(value)?;
    if ["/chat/completions", "/responses", "/messages"]
        .iter()
        .any(|suffix| url.path().ends_with(suffix))
    {
        return Err("Enter the API base URL, without /responses, /messages, or /chat/completions. Codex requires the Responses API".into());
    }
    Ok(value.into())
}

#[cfg(test)]
#[path = "custom_providers_tests.rs"]
mod tests;
