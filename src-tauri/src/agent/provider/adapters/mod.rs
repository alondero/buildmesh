//! Concrete `AgentProvider` adapters — one file per provider.
//!
//! Adding a new provider:
//! 1. Create a new `<name>.rs` module here implementing `AgentProvider`.
//! 2. Expose a `pub static <NAME>: <NameAdapter> = <NameAdapter>;`
//! 3. Add a `Provider::<Name>` enum variant in `models/mod.rs`.
//! 4. Add the `Provider::<Name> => &adapters::<NAME>` arm in `Provider::adapter()`.
//!
//! Note: MiniMax and Kimi have **no** adapter here. They are Claude Code with a
//! swapped backend, so they run as dynamic harness profiles whose paired model-
//! provider account supplies `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` at
//! spawn time (see `preferences::resolve_provider_env`, issue #538) — the
//! `anthropic` adapter is the executor for every Claude-compatible endpoint.

pub mod agy;
pub mod anthropic;
pub mod codex;
pub mod grok;
pub mod opencode;
pub mod terminal;

pub use agy::AGY;
pub use anthropic::ANTHROPIC;
pub use codex::CODEX;
pub use grok::GROK;
pub use opencode::OPENCODE;
pub use terminal::TERMINAL;

