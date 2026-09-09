# Run 85: review handoff timeout

Investigated on 2026-09-09 from base
`40cf36c34cd3e732e08c226b059d2f8ab19060c0`. Evidence came from read-only
queries of the stable profile's SQLite ledger, its application log, and
agent 3759's Claude Code transcript. The stable process and ledger were
not changed.

## What happened

Run 85 attached the built-in local review preset to agent 3759 at 10:56:12
UTC. The reviewer provider snapshot was `agy`. Its only steps were `trigger`
and `await_source`; it never reached reviewer scheduling or spawning.
The run failed at 12:39:31 with a 15-minute no-progress timeout.

Two independent faults prevented the handoff:

1. **Printed source code created a phantom background task.** The implementation
   was refactoring the transcript reader itself. Its Read results contained
   the launch example `ID: xyz` and the background notification promise.
   The background-task scan treated these fragments anywhere in a tool result
   as an actual launch. No completion notification could arrive for `xyz`.
   The log records completion hooks being suppressed as background waits at
   12:20:16 and 12:21:16. The lost-turn watchdog subsequently changed the
   agent to `awaiting_input`; terminal output resumed and cleared that state
   at 12:23:19, before another watchdog yield at 12:24:20.
2. **Review readiness was classified as task completeness.** `await_source`
   used the ordinary implementation classifier even for a clean `Ready`
   lifecycle state. Its persisted final report described 11 implementation
   commits and remaining snapshot tests. It was classified `working`, an
   outcome with no outgoing route from this gate. Report deduplication then
   correctly avoided repeating the same classification, but the circuit had
   no productive next action and expired. The saved preset's `await_fixes`
   gate had the same task-completeness requirement.

There was also one 30-second classifier timeout at 12:21:50, followed by a
successful `working` result at 12:22:30. This recovered failure was not a
reviewer spawn failure. Capacity and the configured reviewer adapter were
not reached. A previous `CONTINUE` response also became `WORKING` when its
required delivery evidence was absent; borrowed source ownership correctly
prevented unsolicited continuation. Increasing the timeout would not repair
either root cause.

## Changes

- Recognize background launches only from the harness's anchored launch
  envelope, followed by its output-path field and completion promise. Read
  and grep output containing source examples no longer creates tasks. Both
  supported launch forms and real pending-task suppression remain covered.
- In the built-in local review preset, a clean `Ready` or `Completed` lifecycle
  state plus a fresh nonempty report completes a source turn gate without an LLM. The reviewer decides
  whether the work is complete and correct. Ambiguous input-wait states use
  a dedicated review-readiness prompt, distinguishing finished partial work
  from permissions, human questions, API failures, and ongoing background work.
- New presets use `AwaitAgentTurn` for fixes. Existing saved presets apply the
  same policy to their source-bound classifier without rewriting active graphs.
  User-authored task classifiers retain their task-completion contract.
- A clean lifecycle completion can reconsider an unchanged report previously
  parked as `working`. Fresh-report selection, pre-feedback report baselines,
  process/input stamps, and terminal-run guards still apply.
- Reviewer approval still requires an explicit review verdict. Findings are
  delivered to the source, the reviewer is retired, and a new review starts
  after the next finished fix turn. Retry limits and attention checkpoints
  remain bounded; finishing a reviewer turn alone never means approval.

## Evidence and rollout

The initial two focused regressions failed before the fixes. After the fixes,
all five focused tests passed, including a temporary replay of run 85's entire
transcript through the production background counter: zero pending tasks.
That local replay test was removed; portable source-output fixtures remain.
The hook-route regression passes the real file-backed counter to the production
hook decision and asserts `Ready`.

The worker tests cover clean completion with an unavailable classifier,
permission/background/error states, unchanged reports across restart and
completion, cancellation, and three review rounds for both new and saved
presets. They exercise production policy and stepper transitions, with mocked
external verdicts and acknowledged spawn/cleanup effects. They do not launch
real reviewer agents or prove a live provider's availability.

Final `scripts/check.ps1 rust -SerialRust` passed on Windows: 3,000 library
tests and 18 integration tests; 14 library tests and one doctest ignored.
The wrapper also passed all six agent-infrastructure tests and diff checks.
`npm run build:mobile` passed. Generated bindings were unchanged. Initial
full-suite failures exposed a test timing mistake (approval finishes on the
following Tick) and a schema snapshot EOF-whitespace mismatch; both were
corrected before the final full run. The schema snapshot helper now emits a
canonical single final newline, so the comparison remains exact.

`cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets`
completed successfully with warnings (13 library warnings; 38 test-target
warnings, including duplicates). Comparing diagnostic locations with the
final diff found none on changed lines. A strict `-D warnings` run was not
performed. No frontend behavior changed, and no live reviewer session or
browser end-to-end run was started during verification.

Independent Standards and Spec reviews found missing lifecycle-order coverage
and an overly broad custom-gate behavior change. The final tests cover the
former, and the readiness policy now requires the built-in review preset's
context flag and an explicit borrowed-source target. Custom task-completion
gates keep their original semantics.

The source changes require a rebuilt application and restart. Failed run 85
is retained as historical evidence; starting a new review of its preserved
source is the recovery path. This change does not mark its code approved or
automatically restart its implementation session.
