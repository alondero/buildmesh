# ADR 0055: Circuit control workflows live behind one transport-independent module

Status: accepted

## Context

Issue [#2673](https://github.com/alondero/buildmesh/issues/2673) reviewed the
Circuit control surface and found the workflow knowledge split across the two
transport adapters.

`src-tauri/src/commands/circuit.rs` owned the choreography: the shared update
emission, the worker wakeup after an outcome or a recovery, the cancellation
barrier (invalidate the effect batch, quiesce in-flight spawns, persist the
cancellation, release the source, retire the helper agents, emit the state),
the deletion barrier, the Start Review eligibility check and the evaluator
registration, and the review continuation's process resumption.

`src-tauri/src/http/routes/circuit.rs` imported that command module and
called its helpers — and then appended the outcome and recovery notifications
*itself*, because the command helpers only persisted and woke the worker. The
phone therefore had a second copy of the "tell the windows" step, and the two
copies had to be kept in step by hand. `services/circuit_prompt.rs` reached
up into the command layer for the same emit.

The risk was not a reproduced bug. It was that changing a workflow required
understanding behaviour owned by a command adapter, and that the post-call
choreography lived in the caller. The two existing transports — the desktop
commands and the mobile routes — are a real seam, and the seam had no owner
on the domain side of it.

## Decision

Create `src-tauri/src/services/circuit_control/`. It owns the named Circuit
operations: `record_outcome`, `recover_run`, `push_step`, `approve_step`,
`set_round_limit`, `pause_run`, `resume_run`, `start_review`,
`continue_review`, `cancel_run`, `cancel_runs` and `delete_circuit`, plus the
Circuit catalog reads a second boundary needs (`list_circuits`,
`list_circuits_with_runs`, `list_queue`, `list_probe`, `list_runs`,
`list_agent_ownerships`).

Each operation owns its complete coordination. A committed operation owns the
required wakeup, the registration and the notification, and no success
notification precedes the persistence it belongs to. The adapters keep only
what a transport needs: the desktop commands keep the offload onto the
blocking pool and flatten a typed failure into the Tauri wire's `String`; the
HTTP routes keep authentication, parsing, the path/body identity check, the
offload and the status conversion.

Ownership boundaries that do not move:

- Persistence stays in `db/circuit`.
- Pure decisions stay in `circuit`; the stepper remains pure.
- Observation and effect execution stay in `services::circuit_worker`.
- The wire DTO declarations stay in `commands/circuit.rs` (ADR 0047), so the
  generated TypeScript and the ts-rs export names do not move. The control
  module imports those declarations as data shapes and invokes no Circuit
  command — the same exception ADR 0047 grants the PR workflows.

The application handle stays optional throughout: a process whose handle is
not installed yet has no window to tell, and the mobile route reads the handle
it already owns out of `http::state`.

`scripts/check-circuit-command-ownership.mjs` (with
`tests/agent-infra/circuit-command-ownership.test.mjs`) fails the build when
a route, a service module or the companion CLI calls a Circuit *command
implementation*. The guard is deliberately narrow: it forbids calls to the
`#[command]` functions of that one module, permits its DTO declarations, and
leaves the other command modules alone.

## Alternatives and consequences

- **Leave the choreography in the command layer and share it through
  `pub(crate)` helpers.** That is what the code did, and it is what produced
  the duplicated notification: a helper that persists but does not notify
  invites the caller to notify. The seam was real but had no owner.
- **Introduce a `CircuitRepository` / `CircuitRuntime` trait.** There is one
  implementation and one set of concrete facilities to reuse — the blocking
  executor, the process registry, the spawn lifecycle leases, the worker
  quiescence helpers and the application emitter. A trait would have widened
  the interface without a second implementation, which ADR 0047 already
  rejected for the other responsibility modules.
- **Move the wire DTOs too.** That would change the generated TypeScript
  exports and the ts-rs export names for no benefit to this decision, and
  ADR 0047 defers it to its own migration.
- **Extract a crate.** The dependency direction (`services/` → `db/`,
  `circuit/`, `agent/`) does not need one, and a crate would make the pure
  stepper's boundary harder to see, not easier.

The consequence is that the control module is the only place a Circuit
workflow may change, and both transports reach it. The Rust test inventory
moves with the code: the cancellation retirement suite moves from
`commands::circuit::tests` to `services::circuit_control::cancellation::tests`,
because the helper it exercises moved with the workflow it belongs to, and the
module gains an interface suite of its own
(`services::circuit_control::tests`) that drives the operations against an
isolated database. The wire shapes and the generated TypeScript bytes are
unchanged — the `binding-drift` gate and `git status src/types/generated` both
report no change.

## Verification

Compare the baseline and candidate `cargo test --lib -- --list` inventories
and the generated TypeScript bytes. Run the command, HTTP Circuit, recovery,
cancellation and worker suites, the shard-coverage gate, and the repository
verification. The ownership guard must fail when a route calls a Circuit
command implementation, and pass on the candidate.

## Links

- [Autopilot circuits](../development/circuits.md)
- [Rust backend module map](../development/module-map.md)
- [ADR 0047: Backend responsibility modules behind stable entrypoints](0047-backend-responsibility-modules.md)
