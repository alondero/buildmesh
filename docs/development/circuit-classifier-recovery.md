# Circuit classifier recovery investigation

Investigated 2026-09-05 against base `0ea96491b316d35f3244baf4425a6bd418c45a16`.

## Runtime evidence

Read-only queries against the stable profile's SQLite ledger found runs 17 and
27 on Mesh 65 waiting at `implementation_classifier` and `finish_classifier`.
Their completed spawn steps still owned agents 3535 and 3549. Both agents were
`ready`. Run 28 was also active, with its implementer in `pending_slot`.

The log recorded both agents resuming at approximately 19:08 UTC. Their readable
transcripts contained later reports (20:35 UTC for run 17, 20:16 UTC for run 27),
but the ledger still contained the earlier implementation reports. Run 17's old
WORKING verdict followed a real question about the next implementation stage;
it was not evidence that its classifier subprocess was still executing.

Circuit spawn registered evaluator ownership only at creation. Startup recovery
checked durable associations but did not restore their in-memory registrations.
The per-run drive restored only borrowed source agents, not owned agents.
Consequently resumed PTY output was discarded, and both classifier freshness
checks and the lost-turn watchdog lacked observations. Completed spawn steps
must retain evaluator ownership throughout finish, review, and feedback gates.

## Related boundaries

- A classifier's consumed report belongs to that step and attempt. A global
  per-agent evaluation clock cannot identify which gate consumed a turn.
- A transcript can recover a report produced before output capture resumed.
  Classification uses the same report persisted for feedback. Before prompt
  delivery, the worker persists the preceding assistant-record revision in the
  transition commit. The bounded JSONL reader combines record position with
  SHA-256 of the normalized assistant text:
  user/tool activity cannot make an old report fresh, while identical responses
  at different positions remain distinct. OpenCode uses the assistant message's
  SQLite ID and text hash through its existing read-only resolver. An unreadable or unsupported transcript
  cannot authorize a resume redraw; it needs a new live turn or readable report.
- Trimming the bounded PTY buffer must adjust its turn-start offset; an empty
  new turn must not expose the previous turn's completed report.
- A classifier backend failure is not a WORKING verdict. Recovery needs a
  bounded retry cadence and a visible explanation while it waits. Empty or
  suppressed reports are ignored rather than sent to the classifier.
- The reactive PTY path checks node yield state and evaluator freshness before
  opening transcript stores or cleaning terminal output. Evaluator ownership,
  tail, turn boundary, and freshness clocks share one per-node state lock.
  Freshness is keyed by run, gate, and attempt so one downstream classifier
  cannot suppress another; a short bounded probe cooldown covers transcript
  publication that lags the PTY yield.

The existing review blueprint additionally gates review on clean, pushed Git
state and an existing open PR, retries wrap-up corrections, closes reviewers,
and bounds review rounds. These transitions require worker and stepper evidence;
LLM classification alone does not establish that a PR or review succeeded.

The stable app was inspected without changing its ledger or restarting its
processes. Source changes require a rebuilt app before they affect those runs.

Run 28's wait is consistent with circuit 5's two-step concurrency budget: the
two stuck classifier steps occupy that budget. Mesh 65 permits eight agents
and four runs; each of runs 17, 27, and 28 has a two-agent reservation. The
waiting implementer becomes eligible as the existing gates release step slots.

## Verification

- Before fixes, the targeted run executed 112 tests: 109 passed and three new
  regressions failed (lost ownership, rollover truncation, stale empty turn).
- `scripts/check.ps1 rust -SerialRust`: 2,873 library tests and 18 integration
  tests passed; 14 library tests and one doctest were ignored. This includes
  wrap-up verification/retry, PR discovery/replay, reviewer feedback/cleanup,
  review exhaustion, and capacity tests. The integration checks include real
  Windows/WSL PTYs; no real GitHub mutation or LLM classification was performed.
- `npm run test:agent`: six passed. `npm run check:agent -- --base
  0ea96491b316d35f3244baf4425a6bd418c45a16` and `git diff --check` passed.
- `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets`
  completed successfully with warnings. Three warnings in new fixture-writing
  code were corrected; warnings in unchanged code remain (no baseline Clippy
  comparison was run).
- A concurrent focused recheck hit Windows `LNK1104` while the full suite held
  its test executable. Rerunning `cargo test --locked --manifest-path
  src-tauri/Cargo.toml --lib circuit_ -- --test-threads=1` after the full suite
  exited passed all 118 selected tests, including the fixture lint corrections.

Independent Standards and Spec reviews identified stale transcript selection,
short-report handling, error clearing, and provider recovery gaps during the
change. These were corrected and covered by the final tests.
