//! Per-provider [`crate::services::usage::adapter::UsageAdapter`] implementations.
//!
//! A new provider's usage endpoint goes in a new file here — rather than in
//! `usage.rs`, which is orchestration only (issue #2528). The file owns that
//! provider's whole fetch: the credential read, the HTTP request, the response
//! parse and the wording its provider uses for a failure. (`freebuff` is the
//! one adapter whose fetch lives elsewhere:
//! [`crate::services::freebuff_usage`].) `docs/development/providers.md`
//! carries the contributor-facing version of this map.
//!
//! The shared driver in [`crate::services::usage::adapter`] owns the
//! request → status → parse flow for the adapters that use it (`agy`,
//! `commandcode`, `grok`, `minimax`, `opencode`): the adapter supplies the
//! request builder, the parse step and a
//! [`crate::services::usage::outcome::ResponsePolicy`] for the wording its
//! provider uses. `agy` uses the driver for its model-API fallback only; its
//! quota-summary read keeps its own status handling, as do the adapters
//! outside the list above.
//!
//! A provider that needs a second, best-effort read (Command Code's
//! subscription lookup) uses the driver's enrichment rather than bolting on
//! an extra HTTP call.

pub(crate) mod agy;
pub(crate) mod anthropic;
pub(crate) mod codex;
pub(crate) mod commandcode;
pub(crate) mod cursor;
pub(crate) mod deepseek;
pub(crate) mod freebuff;
pub(crate) mod grok;
pub(crate) mod kimi;
pub(crate) mod minimax;
pub(crate) mod muse_code;
pub(crate) mod openai;
pub(crate) mod opencode;
pub(crate) mod openrouter;
pub(crate) mod perch;

pub(crate) use agy::AgyAdapter;
pub(crate) use anthropic::AnthropicAdapter;
pub(crate) use codex::CodexAdapter;
pub(crate) use commandcode::CommandcodeAdapter;
pub(crate) use cursor::CursorAdapter;
pub(crate) use deepseek::DeepseekAdapter;
pub(crate) use freebuff::FreebuffAdapter;
pub(crate) use grok::GrokAdapter;
pub(crate) use kimi::KimiAdapter;
pub(crate) use minimax::MinimaxAdapter;
pub(crate) use muse_code::MuseCodeAdapter;
pub(crate) use openai::OpenaiAdapter;
pub(crate) use opencode::OpencodeAdapter;
pub(crate) use openrouter::OpenrouterAdapter;
pub(crate) use perch::PerchAdapter;
