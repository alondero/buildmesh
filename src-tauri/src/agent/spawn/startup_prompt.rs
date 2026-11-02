//! Ordinary initial prompt delivery; attention feedback belongs to this caller.

use crate::agent::prompt_delivery::{
    self, DeliveryMode, DeliveryRequest, DeliveryStop, ObservationRequirement, PreparedPrompt,
};

pub(super) fn deliver_after_boot(
    app: tauri::AppHandle,
    node_id: i64,
    provider: crate::models::Provider,
    prompt: String,
) {
    let registry = &crate::agent::process::PROCESS_REGISTRY;
    let Some(process) = registry.get(&node_id) else {
        return;
    };
    // Input accepted during launch already supersedes this automatic first turn.
    let stamp = format!("{}:0", process.generation);
    let spawned = std::thread::Builder::new()
        .name(format!("startup-prompt-{node_id}"))
        .spawn(move || {
            let prepared = PreparedPrompt::for_adapter(node_id, provider.adapter(), &prompt);
            let outcome = prompt_delivery::deliver(
                registry,
                provider.adapter(),
                DeliveryRequest {
                    prompt: &prepared,
                    mode: DeliveryMode::OrdinaryStartup,
                    observation: ObservationRequirement::Unobserved,
                    expected_input: Some(&stamp),
                    still_current: &|| Ok(true),
                },
            );
            tracing::info!(node_id, ?outcome, "Initial prompt delivery finished");
            if matches!(
                outcome.stop,
                Some(DeliveryStop::Unverified(_) | DeliveryStop::TransportFailure(_))
            ) {
                crate::commands::attention::mark_attention(node_id, &app);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(node_id, %error, "Could not start initial prompt delivery");
    }
}
