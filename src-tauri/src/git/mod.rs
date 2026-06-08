//! Git access via the `git2` crate.
//!
//! One home for Buildmesh's direct libgit2 usage. Historically each command and
//! service module opened `git2` itself and re-derived the same primitives —
//! "is the repo dirty", ahead/behind vs upstream, the short SHA, the HEAD branch
//! name — with several copies that had already drifted apart (two different
//! dirty-check predicates, two opposite error fallbacks). `primitives` is the
//! single definition those callers now share; the thin `#[command]` adapters
//! stay in `commands/`. See ADR 0007.

pub mod health;
pub mod primitives;
pub mod sync;
pub mod worktree;
