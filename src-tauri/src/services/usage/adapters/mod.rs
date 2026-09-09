//! Per-provider [`crate::services::usage::adapter::UsageAdapter`] implementations.
//!
//! Each file is one drop-in adapter. Thin wrappers that still delegate to the
//! legacy `usage.rs` fetchers are an intentional intermediate step (issue
//! #1657 step 4): the catalog already dispatches through the seam, so the seam
//! is the test surface even before `usage.rs` reaches zero HTTP code. Future
//! commits move each provider's credential-plus-fetch-plus-parse code + its
//! pure `parse_*` tests into its file unchanged, then delete the legacy fn.

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
pub(crate) mod opencode;
pub(crate) mod openai;
pub(crate) mod openrouter;

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
pub(crate) use opencode::OpencodeAdapter;
pub(crate) use openai::OpenaiAdapter;
pub(crate) use openrouter::OpenrouterAdapter;
