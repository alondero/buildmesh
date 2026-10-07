# ADR 0042: Retire the per-circuit shared step budget

Status: proposed.

Issue: [#2114](https://github.com/alondero/buildmesh/issues/2114)

Date: 2026-10-07

Partially supersedes [ADR 0028](0028-circuit-run-capacity-contract.md):
it reverses the retention of the per-circuit running-step budget
(`autopilot_circuits.concurrency_limit`) as a scheduling input. Everything
else in ADR 0028 still stands — mesh-level run admission via
`meshes.circuit_run_capacity`, per-run agent-lease reservations, and the
optional app-wide agent pool as the only process-level backstop.

## Context

Circuits currently have three capacity budgets: mesh run admission
(`circuit_run_capacity`), the per-circuit step budget
(`autopilot_circuits.concurrency_limit`), and the optional app-wide agent
pool. The middle one is scoped per circuit, not per run: it bounds running
steps across *all* of one circuit's `running` + `paused` runs
(`db::count_running_circuit_steps`).

That scope is the surprise. On a mesh running one review circuit with a
step budget of 2, three admitted runs time-share 2 step slots, so the third
run's step parks with "all 2 of this circuit's step slots are busy" no
matter what the mesh-level "concurrent circuit runs" setting says. The mesh
knob reads as the concurrency contract; the circuit knob silently overrides
it from a different surface (the blueprint editor header). When every
circuit on a mesh fans out the same way, the step budget behaves as a
hidden second run cap with no independent tuning value.

The scope was inherited, not decided. ADR 0028 documents it as pre-existing
fact ("currently defaulting to 1 ... across a single circuit's
`running`/`paused` runs") while introducing the run cap around it.

The blueprint "floor" (review needs 2, skeleton needs 1) does not rescue the
design. It is a validation bound, not a scheduling guarantee: it only
appears in `clamp_concurrency_limit`, which keeps a user-supplied ceiling
inside `[floor, MAX]`. The scheduler never reads the floor — it reads only
the ceiling. Worse, the floor exists so that *one run* can overlap
implementer and reviewer, but circuit-wide sharing lets a sibling run hold
the slot the floor was meant to guarantee. The mechanism serves neither
per-run correctness nor a legible total.

## Decision

Remove the per-circuit step budget as a scheduling input. Within an
admitted run, steps are gated only by DAG eligibility (predecessors
complete) plus the existing agent lease / pool accounting. The only
user-facing concurrency ceiling becomes concurrent circuit runs per mesh.

Invariants:

1. **Eligibility-only step gating.** `schedule_ready` starts every eligible
   node; edges serialize, branches parallelize. No run can stall on step
   slots because there are no step slots.
2. **The lease still prevents intra-run agent deadlock.** Admission
   reserves each run's declared `SpawnAgentNode` footprint up front (the
   issue #1467 fix), so a run's own steps never block each other on agent
   accounting. Pool pressure delays agent steps; completing steps free it.
3. **Run admission is unchanged.** `circuit_run_capacity` still bounds
   admitted (`running` + `paused`) runs per mesh, `pending` still does not
   count, and a paused run still retains its run slot.
4. **No per-circuit ceiling of any scope remains.** Neither shared across
   runs nor per run. Blueprint overlap needs (implementer + reviewer) fall
   out of the graph instead of a stored constant.

## Alternatives considered

1. **Re-scope the budget per run** (each run gets its own N step slots).
   Rejected: it keeps two user-facing knobs for one mental model
   ("concurrent runs" plus "steps per run") and reintroduces the same
   discoverability problem in miniature. The review floor finally meaning
   what it says does not outweigh that.
2. **Keep the shared budget and surface it better** (effective-concurrency
   notes, actionable queued copy). Rejected: better copy around a throttle
   the user did not ask for. Surfacing mitigates confusion without
   removing the redundant control.
3. **Derive a per-run width from the DAG** (widest antichain) instead of a
   stored floor. Rejected as unnecessary: with no ceiling there is nothing
   to clamp, so no width needs computing. The scheduler already computes
   eligibility per tick.

## Consequences

**Fan-out backstop.** Removing the ceiling removes the only default brake
on intra-run fan-out: a hand-built graph with many parallel spawns fires
them all at once, bounded only by the agent pool when one is configured.
Accepted: pathological graphs are the author's doing and visible on the
canvas; the pool remains available for hosts that need a hard backstop.
If this proves wrong in practice, the follow-up is a default pool — not a
per-circuit budget.

**Removal checklist** (code wins over this list if they disagree):

- Rust model: `min_concurrency_limit`, `default_concurrency_limit`,
  `clamp_concurrency_limit`, `MAX_CONCURRENCY_LIMIT` on
  `CircuitBlueprintKind`; catalog `min/default_concurrency_limit` fields
  and their drift tests in `blueprint_contract`.
- Commands: `update_circuit_concurrency_limit` (plus locked variant) and
  its `lib.rs` handler registration.
- Persistence: `set_autopilot_circuit_concurrency_limit_inner`; decide the
  fate of the `autopilot_circuits.concurrency_limit` column (stop reading
  it vs a drop migration) in the implementing issue.
- Scheduler: the circuit-wide running-step count, `queued_step_bind`'s
  step-slot branch, `CapacityBind::CircuitStepSlots`, and
  `circuit_free_slots` in the tick `Capacity`.
- Frontend: the Step slots select in the circuit editor, the step-slot
  branch of `queuedReason`, and the `concurrencyLimit` plumbing in
  `CircuitCapacity`; regenerate TS types.
- Tests: copy/clamp/catalog/stepper/capacity unit tests, probe and
  flow-editor suites, and integration shots that seed `concurrency_limit`
  rows.

**Verification.** Full Rust suite (`node scripts/rust-test-shards.mjs`),
`scripts\check.ps1 all-ts` (or `npm run build`, `npm run lint`,
`npm test` elsewhere), `npm run check:docs`. The "Waiting for a slot"
step-slot copy must have zero remaining producers; a circuit with three
admitted review runs must show all three progressing, bound only by run
capacity and pool.
