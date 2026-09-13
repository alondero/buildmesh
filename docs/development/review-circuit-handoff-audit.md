# Review circuit handoff audit

Investigated on 2026-09-13 against
`1c4e05eef194b6ead40424dc6299fcbb95e33abb`.

## Observed failure

The stable profile was inspected through SQLite `mode=ro`. Run 122 was running
at `await_source`, borrowing Codex node 4006. The node was still recorded as
`running`, with its last lifecycle transition at 18:25:20 UTC. Its matching
parent rollout ended with a final answer and native `task_complete` at
18:27:33.252 UTC. The completion event identified the same turn that started
at 18:25:20.591 UTC. No later turn followed in the inspected rollout.

Run 123 was also waiting for a Codex source, but its inspected rollout still
contained tool activity and commentary. An assistant message alone cannot
authorize review: those two observations must produce different decisions.

No live run, agent, preference, or transcript was modified during inspection.

## Root causes and fixes

1. Gate classification first requires a yielded lifecycle status. A missed
   completion hook leaves a completed source outside that gate indefinitely.
   The watchdog previously required PTY silence plus a separate LLM judgement
   even when the harness had recorded explicit completion. Codex's transcript
   adapter now supplies native completion evidence. The watchdog probes it
   independently of PTY silence and classifier availability, then publishes
   completion through the existing lifecycle and Node Turn paths.
2. The initial review handoff required a transcript or a trustworthy live PTY
   boundary even when the source was already Ready or Completed. A regression
   through `classify_step_turn` reproduced that failure. The initial built-in
   source gate now accepts clean lifecycle completion without report text;
   the reviewer still inspects the source working directory. A saved prompt
   boundary prevents this exception from accepting an earlier feedback turn.
3. Recovery must not overwrite newer work. Independent review identified
   validation/publication races. Native completion now rejects later activity,
   partial records, mismatched turns, malformed timestamps, newer sessions,
   drafts, and newer submitted input. The final DB update compares the complete
   observed lifecycle stamp while registry and writer locks exclude process
   replacement and input changes. It emits lifecycle events only after the
   conditional update succeeds and those locks are released.

## Evidence and limits

The new reportless-handoff test failed before its fix, then passed with the
existing review-loop tests. Tests exercise native JSONL file reading, bounded
tails, stale observations, the real process input registry, a private SQLite
lifecycle update, and scheduling the reviewer through the production stepper.

Verification on Windows:

- `scripts/check.ps1 rust -SerialRust`: 3,328 library tests and 18 integration
  tests passed; 19 library tests and one doctest were ignored. The wrapper's
  six agent-infrastructure tests also passed.
- The reportless source regression failed before its handoff fix. Focused
  review-handoff tests then passed (six tests), as did native-completion
  checks (three tests) and the process/SQLite recovery check (one test).
- The final `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib
  circuit_ -- --test-threads=1` recheck passed all 174 selected tests.
- `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets`:
  completed with warnings in untouched files; no warnings remain in changed
  files. No baseline Clippy comparison was run.
- `npm run check:agent -- --base 1c4e05eef194b6ead40424dc6299fcbb95e33abb`
  and `git diff --check` passed. Independent Standards and Spec reviews were
  performed; the material findings were fixed before handoff.

This does not establish a 100% end-to-end success rate across external agent
services. Native transcript recovery currently supports Codex; other harnesses
retain their existing lifecycle watchers and fallback recovery. Permission
requests, unrecognized verdicts, failed launches, missing worktrees, classifier
outages, and feedback delivery still have their existing explicit outcomes.
No live agent/GitHub review cycle was launched during verification. Existing
running application processes require a rebuilt application before these
source changes affect them.
