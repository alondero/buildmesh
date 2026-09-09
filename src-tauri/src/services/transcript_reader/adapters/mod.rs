//! Per-harness `TranscriptAdapter` implementations (issue #1661).
//!
//! One adapter per harness id that has its own transcript format. The
//! catalog in [`super::super::adapter`] holds drop-in `&'static` references;
//! adding harness N means adding one file here and one entry to that
//! catalog — not editing four parallel `match` tables in the reader and
//! three in the attention route.

pub mod agy;
pub mod claude_code;
pub mod codex;
pub mod commandcode;
pub mod cursor;
pub mod grok;
pub mod opencode;

pub(crate) use agy::AgyAdapter;
pub(crate) use claude_code::ClaudeCodeAdapter;
pub(crate) use codex::CodexAdapter;
pub(crate) use commandcode::CommandCodeAdapter;
pub(crate) use cursor::CursorAdapter;
pub(crate) use grok::GrokAdapter;
pub(crate) use opencode::OpenCodeAdapter;