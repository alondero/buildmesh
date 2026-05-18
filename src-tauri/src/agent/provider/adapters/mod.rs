//! Concrete `AgentProvider` adapters — one file per provider.
//!
//! Adding a new provider:
//! 1. Create a new `<name>.rs` module here implementing `AgentProvider`.
//! 2. Expose a `pub static <NAME>: <NameAdapter> = <NameAdapter>;`
//! 3. Add a `Provider::<Name>` enum variant in `models/mod.rs`.
//! 4. Add the `Provider::<Name> => &adapters::<NAME>` arm in `Provider::adapter()`.

pub mod anthropic;
pub mod gemini;
pub mod minimax;
pub mod opencode;

pub use anthropic::ANTHROPIC;
pub use gemini::GEMINI;
pub use minimax::MINIMAX;
pub use opencode::OPENCODE;
