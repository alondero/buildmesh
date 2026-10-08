//! Autopilot Circuits — the composable trigger-action graph feature
//! (spec #1205, walking skeleton #1206).
//!
//! Sub-modules:
//! - [`model`] — the Graph Blueprint AST (serialised as
//!   `autopilot_circuits.graph_json`).
//! - [`vocabulary`] — run/step ledger strings, `Queued` ↔ `pending_slot`,
//!   terminal predicates (issue #1660).
//! - [`capacity`] — admission + step-slot + pool arithmetic (ADR-0028).
//! - [`context`] — Mustache-style template context (`circuit.*`,
//!   `node.*`; milestone-2 namespaces resolve empty today).
//! - [`stepper`] — the pure decision core: `advance(run, event) →
//!   (writes, effects)`, unit-tested with no DB or network.
//!
//! The impure seam (worker thread + effect execution) lives in
//! `services::circuit_worker`.

pub mod capacity;
pub mod context;
pub mod model;
pub mod model_node_review;
pub mod observation;
pub mod stepper;
pub mod vocabulary;

#[cfg(test)]
mod blueprint_contract;

#[cfg(test)]
pub(crate) mod test_support;

pub(crate) mod classifier;
pub mod compatibility;
pub(crate) mod delivery;
pub mod evaluator;
pub mod finish;
pub(crate) mod handoff;
pub(crate) mod issue_dependencies;
pub(crate) mod launch;
pub mod security;
pub(crate) mod verification;
