//! Unified session naming module.
//!
//! `engine` owns the asynchronous rename state machine, `slug` owns validation
//! and random-name pools, `repository` isolates persistence, and `wire` owns
//! emitted payloads. This facade preserves the established
//! `session_naming::*` API.
//!
//! State: one naming-state entry per node behind a single map. It
//! folds together what used to be four parallel statics — a PTY-output buffer,
//! a "buffering ready" gate, a "rename in progress" flag, and a failed-attempt
//! counter. Keeping a node's whole naming state in one entry under one lock
//! means it can be read at once, and removes the cross-map self-deadlock hazard
//! that four non-reentrant mutexes invited (a lock can't deadlock against
//! siblings that no longer exist). The fields:
//! - `buffer` — PTY output accumulator; an entry exists = the node hasn't been
//!   renamed yet.
//! - `buffering_ready` — the gate. PTY output is only accumulated after the
//!   *first* Node Turn, which discards the chrome Claude Code prints on
//!   startup (the "Bypass Permissions" warning, plugin/skill listings, banner
//!   repaints) so the renaming LLM never sees that text. Without this gate the
//!   LLM kept producing names like `bypass-permissions-worktree`.
//! - `renaming` — guards against duplicate concurrent LLM calls.
//! - `attempts` — per-node failure counter; caps retries at MAX_RENAME_ATTEMPTS.
//!
//! The LLM rename never holds the `NAMING` lock across its `.await`: the buffer
//! is snapshotted under the lock and released before the network call.
//!
//! Lifecycle:
//! - `on_spawn()` generates an initial random name
//! - `on_output(node_id, data)` buffers PTY output once the node's gate is open
//! - `on_turn(node_id, app)` flips the buffering gate (first call) and triggers
//!   LLM rename when the buffer is sufficient (subsequent calls)
//! - `cleanup(node_id)` removes all state for a node
//!
//! Diagnostic: set `BUILDMESH_DUMP_NAME_BUFFER=1` to dump the raw + cleaned
//! buffer to `%TEMP%/bm-name-buffer-<node_id>-<ts>.txt` whenever a rename runs.

mod engine;
mod repository;
mod slug;
mod wire;

pub use engine::{buffers_size_bytes, cleanup, on_output, on_turn, reset_buffers};
#[allow(unused_imports)]
pub(crate) use engine::{
    naming_backend_env, naming_backend_env_with, resolve_claude_binary, user_renamed_mid_flight,
    ANSI_ESCAPE,
};
#[allow(unused_imports)]
pub use slug::{is_default_name, issue_node_name, on_spawn, pr_node_name, slugify_issue_title};
pub use wire::{NamingBackendFailedPayload, NodeRenamedPayload};

#[cfg(test)]
mod tests;
