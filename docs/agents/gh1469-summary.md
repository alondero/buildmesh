# Issue #1469 — Blueprint contract coverage

## Shipped catalog (single source of truth)

Two built-in blueprints:

- `WalkingSkeleton` (`src-tauri/src/autopilot/circuit/model.rs::walking_skeleton`) — the canonical
  spawn → inject → notify chain under one of four trigger roots (Manual / Interval / labelled GitHub
  issue / labelled GitHub PR). Reconciles spec #1205's *Issue-Driven PR Flow* and *Continuous Looping
  Pacer* presets (they're the same skeleton with different triggers).
- `IssueDrivenAutopilotReview` (`src-tauri/src/autopilot/circuit/model.rs::issue_driven_autopilot_review`)
  — the *PR Adversarial Reviewer* preset. Issue-label trigger, collaborator gate, implementation
  agent, finish path, PR open, reviewer spawn, feedback inject into implementation, close reviewer.

## Drift gates (adding a new built-in blueprint fails CI without a fixture)

- Rust: `autopilot::circuit::blueprint_contract::built_in_catalog_covers_every_blueprint_kind`
  iterates every `CircuitBlueprintKind` variant and asserts a `BUILT_IN_CATALOG` entry exists.
- TypeScript: `tests/unit/circuits-probe-catalog.test.tsx` iterates `PROBE_CATALOG` and asserts each
  matches a generated `CircuitBlueprintKind` variant AND each appears as a `<option>` in the
  blueprint selector.

## Contract test matrix

| Concern                          | File                                                              | Coverage |
| -------------------------------- | ----------------------------------------------------------------- | -------- |
| Builder marker / topology / edges | `src-tauri/src/autopilot/circuit/blueprint_contract.rs`           | 19 tests |
| JSON round-trip / validate       | `blueprint_contract.rs` (every entry iterates `validate()`/`to_json`/`from_json`) | covered |
| Stepper success path             | `stepper.rs::tests::issue_review_blueprint_runs_reviewer_feedback_and_closes_reviewer_node` | covered |
| Stepper collaborator Blocked     | `stepper.rs::tests::issue_review_collaborator_gate_blocks_until_human_approval` | new |
| Stepper implementation → PR → reviewer | `stepper.rs::tests::issue_review_implementation_completion_spawns_reviewer_after_pr` | new |
| Stepper reviewer output          | `stepper.rs::tests::issue_review_reviewer_output_is_captured_into_node_context` | new |
| Stepper feedback targets impl    | `stepper.rs::tests::issue_review_feedback_injection_targets_the_implementer_not_the_reviewer` | new |
| Stepper close_reviewer           | `stepper.rs::tests::issue_review_close_reviewer_emits_close_agent_targeting_the_reviewer` | new |
| Stepper retry exhaustion         | `stepper.rs::tests::issue_review_retry_exhaustion_runs_complete_notify_not_another_loop` | new |
| Stepper retry re-arm             | `stepper.rs::tests::issue_review_retry_completed_reamps_finish_step_with_incremented_attempt` | new |
| Stepper wrapup retry exhaustion  | `stepper.rs::tests::issue_review_wrapup_retry_exhaustion_terminates_without_reviewer` | new |
| Worker seam — close_retry        | `circuit_worker.rs::tests::{walking_skeleton_close_retry_observer_emits_nothing,review_blueprint_close_retry_observer_stops_after_target_clears}` | new |
| Worker seam — spawn recovery     | `circuit_worker.rs::tests::walking_skeleton_spawn_recovery_treats_missing_agent_as_never_attached` | new |
| Probe UI catalog                 | `tests/unit/circuits-probe-catalog.test.tsx`                      | 5 tests |
| Probe UI wire shape              | `tests/unit/circuits-probe-catalog.test.tsx` (per-blueprint `create_circuit` shape) | covered |

## Acceptance criteria status

- ✅ One canonical source of truth (`BUILT_IN_CATALOG`)
- ✅ Every shipped blueprint has Rust model/validation/persistence/stepper + worker-seam coverage
- ✅ Every selectable blueprint has TypeScript Probe coverage using generated wire types
- ✅ Tests assert observable graph/state/effect behaviour, not internal call graphs
- ✅ Review blueprint test proves reviewer is reached only after successful implementation/PR path
- ✅ Failure / blocked / retry / cleanup paths covered
- ✅ Three-preset discrepancy resolved (WalkingSkeleton subsumes two presets)
- ✅ Test suite fails without explicit fixture when a new blueprint is added
- ✅ #1205 + knowledge primer updated
