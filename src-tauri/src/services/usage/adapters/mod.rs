//! Per-provider [`crate::services::usage::adapter::UsageAdapter`] implementations.
//!
//! Each file is one drop-in adapter. Thin wrappers that still delegate to the
//! legacy `usage.rs` fetchers are an intentional intermediate step (issue
//! #1657 step 4): the catalog already dispatches through the seam, so the seam
//! is the test surface even before `usage.rs` reaches zero HTTP code. Codex
//! owns its fetch and parse in `codex.rs` (issue #1672); other providers still
//! migrate one file at a time.

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
pub(crate) use muse_code::MuseCodeAdapter;
pub(crate) use opencode::OpencodeAdapter;
pub(crate) use openai::OpenaiAdapter;
pub(crate) use openrouter::OpenrouterAdapter;
