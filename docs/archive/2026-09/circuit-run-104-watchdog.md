# Circuit run 104: background review mistaken for a finished turn

## Evidence

Inspected the stable application's SQLite ledger and `buildmesh.log` read-only,
plus Antigravity conversation `43352b28-8b78-4be7-903c-93e33c2f47e1`.
All times below are UTC on 2026-09-12.

- 12:32:29: run 104 spawned reviewer agent 3923 using `agy`.
- 12:34:59: transcript step 72 reports test task `task-62` as `RUNNING`.
  Nine of thirteen tests had passed; `turbo_boost_e2e` was running.
- 12:35:00: step 73 says: "The tests are progressing (1-9 passed, including
  `controller_accessories`, `haptics_race_e2e`, `eeprom_exit_flush`, and
  `controller_pak_rom_filesystem`). Waiting for the final e2e tests to finish."
- 12:36:00: the circuit watchdog records 60,011 ms of quiet and synthesizes
  an awaiting-input transition. There is no reviewer completion webhook in
  the log at this boundary.
- 12:36:08: the verdict classifier returns `BLOCKED`; the process is deliberately
  killed. The ledger saves the progress message as `node.reviewer.output` and
  `node.verdict.evaluated_output`, then records the run as failed.

## Cause and comparison with the hook change

At base commit `dd3bcfad`, the watchdog publishes a turn based only on process
liveness, 60 seconds of PTY silence, and `Running` status. Observation treats
the resulting `AwaitingInput` status as a successful `AgentFinished` event.
The review gate then receives a progress message as though it were a final
report. Its existing contract classifies incomplete reviews as `BLOCKED`.
Attention autoclear cannot repair this: the failure path stops the reviewer
before it can produce the output that would clear the false attention mark.

Hook-normalization commit `619eea5c` leaves this watchdog path unchanged.
Its lifecycle ordering fixes and background-running status updates cannot
prevent the watchdog from synthesizing another turn after quiet background
work. Rebuilding that change alone is insufficient.

## Recovery boundary

Quiet time makes an agent eligible for inspection, not completion. Recovery
uses a separate turn-readiness classifier before publishing attention.
Only a final report or an explicit need for input can publish a recovered
turn. Background progress, empty reports, unavailable classifiers, and
unexpected continuation results leave the agent running. Review verdict
classification remains separate: its `WORKING` means changes requested,
not ongoing background work.

Recovery checks are throttled to once per minute. Before publishing, they
recheck process liveness, quiet time, lifecycle and input stamps, and report
revision so activity during classification invalidates the observation.
Existing active-wait deadlines still bound an agent that never finishes.

The regression exercises the stock review graph with the recorded report,
asserts that background/unknown evidence leaves its reviewer running without
opening the verdict gate, and then releases that gate on a final report.
Classifier responses are injected in deterministic tests; those tests prove
routing and lifecycle behavior, not model accuracy. A direct local classifier
probe was attempted but failed because its OAuth session had expired.

## Verification

- Before the guard, the regression failed at the publication boundary with
  the original unconditional recovery behavior (one test, one failure).
- Focused `cargo test --lib watchdog_ -- --test-threads=1`: seven passed.
- Final `scripts/check.ps1 rust -SerialRust`: 3,169 library tests and 18
  integration tests passed; 18 library tests and one doc test were ignored
  by their existing declarations. Six agent-infrastructure tests also passed.
- `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --lib --tests
  --message-format=short`: completed successfully with existing warnings;
  none introduced by this change.
- `npm run check:agent -- --base dd3bcfad` and `git diff --check`: passed.
- Separate standards and specification reviews: no remaining findings after
  correcting the architecture notes and exercising production freshness
  comparisons for lifecycle, input, report, process, output, and status changes.

The stable application was not replaced or restarted, and run 104's ledger
was not changed. This verification does not establish live model accuracy
or constitute a rerun of the original circuit.
