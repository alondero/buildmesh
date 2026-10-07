# Run 86: silent source and missing observer

Investigated 2026-09-10 at base `167cf2b14f0bf075ac3f3ef52e387e2392cf7e7a`.
The stable SQLite ledger and agent transcript were read without changing the
running application, source agent, or run state.

## Evidence

Run 86 belongs to circuit 6 and borrows source agent 3807, using Command Code.
Its reviewer snapshot is `agy`. Its only steps were `trigger` and
`await_source`; the source row remained `running` with a NULL session identity.

Times are UTC:

| Time | Evidence |
| --- | --- |
| 12:51:07.753 | Source launch; durable generation `1789044667753`. |
| 12:51:22.385 | Timestamp inside the matching session header. |
| 12:51:23.317 | Application log: Command Code capture gave up. |
| 13:44:17.878 | Transcript final assistant report: implementation finished and PR created. |
| 13:47:09 | Run 86 attaches to the source. |

The matching session is `d8b8b790-81a7-46c9-892e-f63075728d07`, with the
source worktree in its header. The header timestamp is not proof of its disk
publication time. Installed Command Code 1.53.0 code shows session persistence
at model commits; a session can therefore become readable after capture expires.

## Causes and fixes

1. Capture exhausted its short retry window. Without identity, the transcript
   watcher never started. Circuit identity recovery ran only after a yield,
   although this provider needs the watcher to produce that yield. Recovery
   now runs independently of source status on the existing throttled probe,
   and restores the watcher. Fresh capture also retries delayed publication
   and uses the shared unambiguous, generation-checked identity resolver.
2. The circuit attached after the source's last output. Evaluator registration
   left its output clock empty, so the lost-turn watchdog could never observe
   enough silence. Registration now starts quiet observation once, without
   creating a turn boundary or accepting an old PTY report.
3. Native Command Code committed model records carry `meta.source=model` and
   a stable `meta.messageId`; their outer IDs identify ledger entries. The
   watcher treated these as partial streaming messages and waited for a
   `turn_complete` envelope that the real transcript never writes. Native
   complete responses now yield; legacy split messages still wait for their
   completion evidence. Tool calls do not count as completion.

Late watcher replay also exposed a false-completion risk: an old completion
could be emitted after the reader had already consumed newer user/tool work.
Batch observation now publishes only the current terminal state. Watcher
registration is idempotent, replacement cancels the prior worker, and failed
fresh setup preserves activation so recovery can retry.

## Remaining review path

The saved preset also carries historical model/effort configuration. A reviewer
using another harness could inherit an incompatible source model. Built-in local
reviews now use the selected harness's mesh overrides/application defaults,
ignoring obsolete preset/source snapshots. Authored overrides retain their policy.

The review-loop regression traverses source handoff, AGY provider resolution,
reviewer verdict, feedback, reviewer cleanup, native Command Code fix completion,
two further review rounds, and explicit final approval. Both new and saved
presets are covered. External spawning, input delivery, classification and
cleanup are represented by acknowledged events; this is component/stepper
evidence, not a live provider end-to-end run.

## Verification and rollout

Before fixes, the native completion and late replay regressions failed, as did
the silent-attachment regression. The reviewer configuration regression also
failed before its fix. A temporary test replayed the entire real Run 86
transcript through production identity discovery and the incremental watcher:
the exact identity was recovered, the final completion appeared once, and a
second read emitted nothing. That machine-specific test was removed; portable
regressions remain.

`scripts/check.ps1 rust -SerialRust` passed on Windows: 3,095 library tests,
18 integration tests, and six agent-infrastructure tests. Fourteen library
tests and one doctest were ignored. `npm run build:mobile` passed; generated
bindings were unchanged. Clippy across all targets completed with warnings
outside changed lines (13 library / 40 test-target warnings, including
duplicates). Independent Standards and Spec reviews found no remaining
blocking finding after adding watcher ownership tests.

The configured AGY CLI also answered a bounded print-mode connectivity probe
with `CIRCUIT_PROVIDER_OK`, exit 0. Its warning said plan mode has no effect
when slash-command expansion is disabled. This proves provider connectivity,
not a live review verdict. One overlapping targeted rebuild initially hit
Windows LNK1104 because the full suite still held its test executable; checks
were subsequently serialized.
The final watcher-only rerun passed all 15 tests after the cleanup adjustment.

The source changes require a rebuilt application to affect the stable runtime.
No ledger manipulation has marked Run 86 complete or bypassed reviewer approval.
