# Windows infrastructure fixture timeouts — October 2026

## Scope and method

Issue #2254 reported three agent-infrastructure contract-test scenarios in
`tests/agent-infra/harness.test.mjs` timing out on Windows while
`scripts/check.ps1 all-ts` and `npm run verify` overlapped. The issue asked for
the owning mechanism to be identified and fixed, and for the failures to be
reproduced and re-verified under comparable load.

Repository baseline: `4e79ddd3f5bfbf9f43071ed8de8f7816be4b00be`.
Host: Windows, 24 logical cores, Node v24.12.0.

The three scenarios named in the issue:

| Scenario | Symptom in the issue |
|---|---|
| `real Vitest gate records one exact isolated rerun and keeps unhandled errors red` | expected `FAIL`, received `TIMEOUT` |
| `evaluation accepts a listed isolated pass and still rejects an unlisted flake` | expected child status `0`, received `null` |
| `verify drains both lanes before isolation and resumes only for listed flakes` | expected a frontend failure, received `TIMEOUT frontend-tests` and `TIMEOUT other` |

## Reproduction

The load in the issue was `scripts/check.ps1 all-ts` overlapping
`npm run verify`: two copies of the agent-infrastructure suite, a full frontend
Vitest suite and a Rust compile, on one machine. The issue recorded the
infrastructure suite taking 662.5 s against a 182.77 s sequential baseline, a
3.6x slowdown.

A repeatable stand-in for that load was built from two parts, because the
issue's load had both: CPU workers, which stand for the Vitest suite and the
Rust compile, and process-spawn workers, which stand for the Rust shards and
the CLI subprocesses. On a 24-core host, 48 CPU workers plus 48 spawn workers
reproduce the issue's exact symptom - the evaluation scenario reports
`actual: null, expected: 0`, because the fixture's 30-second wrapper killed a
harness CLI that was still working - while the other two scenarios ran 114.2 s
and 166.1 s against their 60 s gate budgets.

The three candidates the issue named were measured, not assumed.

| Candidate | Measurement | Verdict |
|---|---|---|
| Fixture process startup | A fixture Vitest run spawns 2 node processes (its main process and one worker), not one per core. `VITEST_MAX_WORKERS` changed nothing measurable: 1.03-1.19 s idle and 2.3-5.0 s loaded, either way. | not the owner |
| Worker allocation | As above, and the fixtures are single-file suites, so one worker is one worker. | not the owner |
| Vite/Vitest cache sharing | 12 concurrent fixture runs took a 1.63 s median against the shared `node_modules/.vite` cache and 1.58 s with a private cache. | not the owner |
| Infrastructure scheduling | Node's test runner reached a peak of 23 concurrent test files on this 24-core host - its default is the core count minus one - and 1 on a 2-core runner. | **the owner** |

The mechanism: the agent-infrastructure suite's demand scaled with the machine,
because Node's test runner starts one process per test file with a concurrency
of the core count minus one, and the suite's files are not equally heavy -
several compile Rust or build a Tauri app. A 24-core workstation ran 23 files
at once where a 2-core CI runner ran 1, and two overlapping copies of the
suite doubled that. The fixtures that carry a wall-clock budget then expired on
contention rather than on a defect. The same load on CI, where the runner's
concurrency is 1, never produced the symptom.

## The fix

Two bounds, and neither is a blanket retry.

1. **Scheduling.** `scripts/agent-tests.mjs` is the suite's scheduling owner. It
   caps the file concurrency at 4 - never above the runner's own default, so a
   small runner keeps running as few files as it did - and the suite takes a
   machine-wide agent-test slot, so an overlapping copy waits and says so
   instead of doubling the demand. The slot budget is separate from the
   heavy-gate slots, so the product lanes keep both of theirs.
2. **Budgets.** A fixture gate's budget is a hang detector, not a load
   detector. The budgets are derived from each other rather than chosen: the
   fixture gates get half the enclosing `agent-tests` gate's budget, and the
   fixture's CLI wrapper outlives the gate it wraps by a minute. The
   derivation matters: a nested budget that outlives the enclosing one lets
   the enclosing kill land first, and the nested gate's own attribution - its
   name, its log, the failing test - is destroyed with it.

## Verification

| Check | Before | After |
|---|---|---|
| The three named scenarios, 48 CPU + 48 spawn workers | the evaluation scenario fails with the issue's symptom; the others at 114.2 s and 166.1 s | all three pass, at 155.8 s, 185.6 s and 223.2 s, each inside its own budget |
| The complete suite, unloaded | the issue's sequential baseline was 182.8 s | 422 tests, 415 passed, 7 skipped, 0 failed, 187.8 s |
| The complete suite, 12 CPU + 12 spawn workers (2.3x) | - | 422 tests, 415 passed, 7 skipped, 0 failed, 427.2 s |
| The complete suite, 24 CPU + 24 spawn workers | - | one intermittent failure, in a test outside the three the issue names - see the limits below |
| `npm run verify` | - | 15 of 15 gates PASS |

## Limits

- The reproduction load is calibrated CPU and process-spawn pressure, not the
  issue's literal `all-ts`-plus-`verify` overlap. The two are equivalent in
  kind - both oversubscribe the machine - but not identical.
- The complete suite was not re-run at the heaviest load (48 + 48 workers)
  after the last of the load-sensitive fixtures were fixed. That run is what
  found them. Completeness since then rests on a sweep of the remaining
  wall-clock bounds in the suite's fixtures, not on a green run at that load.
- One residual remains, outside the three scenarios the issue names: at a load
  of 24 CPU plus 24 spawn workers, `a rejected independent review cannot
  complete a green task` fails intermittently. Its completion verdict reports
  a stale receipt where the test expects the review verdict. The harness's
  fingerprint embeds the run identity, and the run identity embeds a toolchain
  probe that runs `cargo --version` under a five-second timeout - a probe that
  expires under load records a different toolchain than one that succeeds, and
  so a different fingerprint. The measurements taken here do not discriminate
  between that and another cause: `cargo --version` measured 278 ms to 2.5 s
  under load, and 25 fingerprint calls in one process returned one tree and
  one identity, so the probe did not expire in that sample. Whether the
  residual predates this change was not demonstrated - the same load was not
  run against the base commit - and this change does alter an input of the
  verdict it asserts on, because it changes the plan's gate definitions. It
  passes unloaded and at the calibrated load, and it is tracked separately in
  [#2581](https://github.com/alondero/buildmesh/issues/2581) rather than fixed here.
- The reproduction scripts were run from the worktree's `.tmp/` directory and
  are not committed; the measurements above are what they produced.
