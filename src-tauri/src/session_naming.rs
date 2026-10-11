//! Unified session naming module.
//!
//! `engine` owns the asynchronous rename state machine, `slug` owns validation
//! and random-name pools, `repository` isolates persistence, and `wire` owns
//! emitted payloads. This facade preserves the established
//! `session_naming::*` API.
//!
//! Naming reads the initial user/assistant dialogue through harness-owned
//! transcript readers on a blocking worker. PTY output is not a naming source.
//! A per-node reservation prevents duplicate inference; unavailable dialogue
//! retries on a later Node Turn without spending the inference retry budget.
//! User-selected names and configured naming backends keep their existing rules.
//!
//! Set `BUILDMESH_DUMP_NAME_BUFFER=1` to dump the selected dialogue and exact
//! naming prompt to `%TEMP%/bm-name-buffer-<node_id>-<ts>.txt`.

mod context;
mod engine;
mod repository;
mod slug;
mod wire;
mod words;

pub use engine::{cleanup, on_turn, reset_buffers};
// Dev-only diagnostic for the `debug_crash_snapshot` command (issue #2153);
// re-exported separately so the gate on the function carries to this list.
#[cfg(any(debug_assertions, feature = "dev-tools"))]
pub use engine::buffers_size_bytes;
pub(crate) use engine::{naming_backend_env, resolve_claude_binary_in, ClaudeSearch, ANSI_ESCAPE};
pub use slug::{
    agent_reviewer_node_name, disambiguate_node_name, is_default_name, issue_node_name,
    issue_reviewer_node_name, on_spawn, pr_node_name, pr_reviewer_node_name,
};
pub use wire::{NamingBackendFailedPayload, NodeRenamedPayload};

#[cfg(test)]
mod tests;
