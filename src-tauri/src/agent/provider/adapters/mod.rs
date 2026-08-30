//! Concrete `AgentProvider` adapters — one file per provider.
//!
//! Adding a new provider:
//! 1. Create a new `<name>.rs` module here implementing `AgentProvider`.
//! 2. Expose a `pub static <NAME>: <NameAdapter> = <NameAdapter>;`
//! 3. Add a `Provider::<Name>` enum variant in `models/mod.rs`.
//! 4. Add the `Provider::<Name> => &adapters::<NAME>` arm in `Provider::adapter()`.
//!
//! Note: MiniMax has **no** adapter here. It is Claude Code with a swapped
//! backend, so it runs as a dynamic harness profile whose paired model-provider
//! account supplies `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` at spawn time
//! (see `preferences::resolve_provider_env`, issue #538) — the `anthropic`
//! adapter is the executor for every Claude-compatible endpoint. Kimi Code
//! (wayfinder #908 / #918) IS a native harness adapter here — Kimi Code's
//! CLI handles its own auth via `~/.kimi/config.toml`, so the `kimi` provider
//! account is now self_auth (issue #918). A user wanting to drive Claude Code
//! against the Moonshot Kimi LLM endpoint can still add it as a custom
//! Claude-compatible account (claude_compatible + base_url + api_key).

pub mod agy;
pub mod anthropic;
pub mod codex;
pub mod cursor;
pub mod commandcode;
pub mod dsh;
pub mod grok;
pub mod kimi;
pub mod mcode;
pub mod opencode;
pub mod terminal;

pub use agy::AGY;
pub use anthropic::ANTHROPIC;
pub use codex::CODEX;
pub use commandcode::COMMANDCODE;
pub use cursor::CURSOR;
pub use dsh::DSH;
pub use grok::GROK;
pub use kimi::KIMI;
pub use mcode::MCODE;
pub use opencode::OPENCODE;
pub use terminal::TERMINAL;


