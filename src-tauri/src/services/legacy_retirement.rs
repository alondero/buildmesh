//! One-way cleanup of persisted legacy automation; never launches agents.
use crate::db;

pub(crate) fn retire_legacy_automation() -> Result<(), String> {
    let pending = db::legacy_retirement::retire().map_err(|error| error.to_string())?;
    for node_id in pending {
        let process_generation = crate::agent::process::PROCESS_REGISTRY.get(&node_id).map(|process| process.generation);
        if !db::legacy_retirement::owns_stop(node_id).map_err(|error| error.to_string())? {
            db::legacy_retirement::acknowledge_stop(node_id).map_err(|error| error.to_string())?;
            continue;
        }
        if process_generation.is_some_and(|generation| !crate::agent::process::PROCESS_REGISTRY.kill_session_if_generation(node_id, generation)) {
            db::legacy_retirement::acknowledge_stop(node_id).map_err(|error| error.to_string())?;
            continue;
        }
        crate::circuit::evaluator::unregister(node_id);
        if let Err(error) = db::legacy_retirement::complete_stop(node_id) {
            tracing::warn!("Legacy automation retired; node {node_id} stop acknowledgement pending: {error}");
        }
    }
    Ok(())
}

pub fn start_retirement_worker() {
    std::thread::spawn(move || loop {
        crate::process_util::run_worker_pass("legacy_retirement", || {
            if let Err(error) = retire_legacy_automation() {
                tracing::warn!("Legacy automation retirement retry failed: {error}");
            }
        });
        std::thread::sleep(std::time::Duration::from_secs(120));
    });
}

#[cfg(all(test, windows))]
#[path = "legacy_retirement/retirement_crash_tests.rs"]
mod retirement_crash_tests;
