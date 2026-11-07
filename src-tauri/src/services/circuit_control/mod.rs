//! Circuit control workflows: the transport-independent owner of every
//! Circuit operation a person (or a phone) can ask for.
//!
//! Issue #2673. The desktop adapter (`commands/circuit.rs`) and the mobile
//! adapter (`http/routes/circuit.rs`) are two doors onto the same operations.
//! The choreography behind one operation — persist the decision, wake the
//! worker, register the evaluator, retire helper agents, tell the windows —
//! used to live in the command layer. That put the parts that come *after* a
//! call in the caller's hands: the phone route appended its own
//! outcome/recovery notification to match the desktop, and a workflow module
//! had to reach up into a command to say "this run changed".
//!
//! This module owns the whole operation. An adapter converts a wire request
//! into a typed request, calls one operation here, and converts the typed
//! failure into its transport's status. Nothing below this module decides how
//! a failure is rendered, and no operation here knows which transport asked.
//!
//! What deliberately stays where it was:
//!
//! - **Persistence** stays in `db/circuit`.
//! - **Pure decisions** stay in `circuit`; the stepper is untouched.
//! - **Observation and effect execution** stay in `services::circuit_worker`.
//! - **Wire DTO declarations** stay in `commands/circuit.rs` (ADR 0047), so
//!   the generated TypeScript and the ts-rs export names do not move. This
//!   module imports them as data shapes; it never invokes a command.
//!
//! The application handle is optional throughout: a process whose handle is
//! not installed yet has no window to tell, and the mobile route reads the
//! handle it already owns out of `http::state`.

mod cancellation;
mod catalog;
mod operator;
mod review;

pub use cancellation::{cancel_run, cancel_run_for_agent_close, cancel_runs, delete_circuit};
pub use catalog::{
    list_agent_ownerships, list_circuits, list_circuits_with_runs, list_probe, list_queue,
    list_runs,
};
pub use operator::{
    approve_step, pause_run, push_step, record_outcome, recover_run, resume_run, set_round_limit,
};
pub use review::{continue_review, start_review};

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
