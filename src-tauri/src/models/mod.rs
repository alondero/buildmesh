//! Data models for Buildmesh.
//!
//! Split by domain (issue #1655). `mod.rs` re-exports every type so
//! `crate::models::Mesh` / ts-rs `export_to` paths stay stable.

mod agent;
mod circuit;
mod git;
mod mesh;

pub use agent::*;
pub use circuit::*;
pub use git::*;
pub use mesh::*;

/// Re-export the wire-level Agent Harness configuration value type from
/// the private `preferences` module so the public API can expose it
/// without leaking the `preferences` module (`preferences` stays private
/// because the rest of its surface is internal-only). The same type is
/// used by the application-level defaults map
/// (`AppPreferences.harness_defaults`) and the spawn-config resolver
/// (`ResolvedAgentConfig`).
pub use crate::preferences::HarnessConfigValue;

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
