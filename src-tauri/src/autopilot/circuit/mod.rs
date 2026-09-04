//! Autopilot Circuits — the composable trigger-action graph feature
//! (spec #1205, walking skeleton #1206).
//!
//! Sub-modules:
//! - [`model`] — the Graph Blueprint AST (serialised as
//!   `autopilot_circuits.graph_json`).
//! - [`context`] — Mustache-style template context (`circuit.*`,
//!   `node.*`; milestone-2 namespaces resolve empty today).
//! - [`stepper`] — the pure decision core: `advance(run, event) →
//!   (writes, effects)`, unit-tested with no DB or network.
//!
//! The impure seam (worker thread + effect execution) lives in
//! `services::circuit_worker`.

pub mod context;
pub mod model;
pub mod stepper;
mod node_review;

#[cfg(test)]
mod blueprint_contract;
