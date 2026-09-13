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
   observed lifecycle stamp while a per-agent input guard excludes input and
   retirement of that incarnation. It emits lifecycle events only after the
   conditional update succeeds and those locks are released.

## Revisions following external review

The original global registry lock across SQLite was a valid contention
finding. It has been removed. Recovery acquires SQLite before the per-agent
input guard, so waiting for the database writer holds no PTY locks. Replacement
and removal retire the observed process under that same per-agent guard before
changing membership. A concrete compare-and-replace operation retries racing
inserts; the registry no longer executes arbitrary recovery callbacks under
its global map lock. Other terminals remain accessible during the DB mutation.

The timestamp gate now accepts both RFC3339 and SQLite UTC timestamps,
including fractional seconds. It still rejects invalid timestamps. Strict
ordering assumes host and WSL clocks are aligned; no tolerance was added,
because accepting an earlier completion after a newer prompt would authorize
stale work. Uncertain native evidence retains the existing fallback path.

Blank lines, including trailing whitespace without a newline, are harmless.
Malformed historical records no longer poison a later explicit completion.
Malformed records *after* the latest completion still invalidate it: they may
conceal a new user turn or tool activity. Ignoring arbitrary trailing corruption
would weaken the freshness guarantee, so that part of the suggestion was not
adopted. Incomplete JSON records remain ineligible until publication finishes.

Native completion is parsed once. A short-lived snapshot checks file size and
modification time before publication rather than rereading and reparsing the
whole tail. This is a read-stability check, not the durable report identity.
Initial handoff uses graph ancestry rather than the literal `await_source` ID.

Follow-up verification reproduced three failures before the fixes: unrelated
registry access timed out during recovery, SQLite-format timestamps were
rejected, and blank/historical damaged lines discarded valid completion.
`scripts/check.ps1 rust -SerialRust` then passed 3,330 library tests and 18
integration tests (19 library tests and one doctest ignored), including the
contention, retirement, timestamp, parsing, metadata, and renamed-gate checks.
Clippy completed with 18 warnings in untouched files and none in changed files.
The final native-completion recheck passed both tests, including same-length
file modification detection.
No additional code-review loop or live circuit run was started; live end-to-end
sign-off remains pending.

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
