//! Application service layer — business logic between commands and DB/IO

pub mod agent_node;
pub mod agent_node_discovery;
pub mod autopilot;
pub mod coordinator_ledger_maintenance;
pub mod fetch_freshness;
pub mod github;
pub mod mesh;
pub mod opencode_oauth;
pub mod pool_worker;
pub mod sync_lock;
pub mod transcript_reader;
pub mod usage;
pub mod warm_pool;
/// `advapi32!CredReadW` / `CredWriteW` / `CredDeleteW` FFI surface — only
/// compiled on Windows; non-Windows callers see "not available" via the
/// `NoCredential` return path inside `services::usage` and
/// `services::opencode_oauth` instead.
///
/// Extracted from `services::usage` for issue #956 so the OAuth dance's
/// `write` / `delete` calls don't have to live inside the usage module. See
/// `docs/knowledge-primer.md` for the credentials section.
#[cfg(windows)]
pub mod windows_cred;
