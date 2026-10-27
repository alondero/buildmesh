# Agent cycle waste: what an hour of Buildmesh agent time actually buys

Reviewed base: `c6cdf2f6bc08ba304472eadacbc5ba4a241f24c4` (27 October 2026).
Scope: MiniMax Code (mcode) sessions run against this repository, the harness
verification ledger, and the gh2599 worktree (`F:\src\buildmesh\.worktrees\gh2599-perffrontend-renderer-compositor-thread-pin`).

This records what happened. It is not a contract.

## Method

The mcode session store holds 2,791 sessions. 250 of them ran with a Buildmesh
working directory; each was replayed from `~/.minimax/v2/sessions/**/messages.jsonl`
to recover the tool calls, their arguments, their result sizes and the wall-clock
gap between a result and the next model turn. The gh2599 worktree supplied the
second corpus: `.harness/events.jsonl` (8 verification attempts, 131 gate events),
`.harness/receipt.json`, `.harness/active-task.json` and the branch reflog.

Wall-clock numbers are tool wall-clock, not session wall-clock. A session that is
left open overnight inflates session duration without any work being done, so the
gate and command durations below are the figures that carry meaning.

## Headline

An agent turn is the unit of cost, and the tool call is the unit of work.
Across the 250 Buildmesh sessions:

| tool | calls |
|---|---|
| bash | 24,341 |
| task_output | 13,183 |
| edit | 7,347 |
| read | 6,792 |
| grep | 3,907 |
| write | 1,499 |

That is roughly 58,700 tool calls, or about 235 per session. Every one of them is
a round trip: the model decides, the tool runs, the result comes back, the model
decides again. The bash calls account for 7,704 minutes (128 hours) of tool
wall-clock on their own, and the two largest buckets inside that are not
computation:

| bash purpose | calls | tool wall-clock |
|---|---|---|
| `gh` CLI | 3,621 | 1,086 min (median 7 s, mean 18 s) |
| `Start-Sleep` | 798 | 671 min |
| `cargo` | 1,603 | 1,039 min |
| `vitest` | 1,679 | 792 min |

Eleven hours of that is agents sleeping. Eighteen hours is one `gh` call at a
time. Both are waiting, and neither is the work.

## The gh2599 worktree

Issue #2599 asks for the renderer Compositor thread to stop being pinned. The
branch amended **the same commit seven times** between 08:00 and 10:25:

```
08:00 ab02b5c0  08:15 27bc0f55  08:36 191b1aaf  08:42 fd0548d6
09:20 a0273ee5  10:10 2baaa9ae  10:25 b32d7f2f
```

Four independent review rounds ran over it, in separate mcode sessions:

| session | tool calls | what it did |
|---|---|---|
| 08:22 | 58 | read the diff, ran the full unit suite, built a throwaway Playwright harness in `%TEMP%` |
| 08:55 | 60 | re-read the diff, ran the full unit suite again, built a second throwaway harness |
| 09:21 | 86 | re-read the diff, ran the full unit suite **twice**, built a third harness plus a `case.ps1` |
| 10:11 | 36 | re-read the diff again |

The unit suite is 339 files and 5,058 tests; it measures 193 s on an idle
machine. It was run by hand at least five times across those four sessions, while
the repo already had `npm run harness -- check`, which selects the checks the
changed paths actually reach.

The verification ledger shows where the rest of the time went:

| attempt | outcome | duration |
|---|---|---|
| 1 | FAIL | 3 s |
| 4 | PASS | 779 s |
| 5 | PASS | 329 s |
| 7 | PASS | 591 s |
| 8 | PASS | 620 s |
| 10 | TIMEOUT | 2034 s |
| 11 | TIMEOUT | 1864 s |
| 12 | TIMEOUT | 904 s |

119 minutes of verification for one change, 78 of them inside attempts that
ended in TIMEOUT. 45 of the 119 minutes went to the Rust lane — `rust-tests`,
`rust-clippy`, `rust-format` — although the change touches no Rust file and no
Cargo manifest. It selects the Rust graph because the diff includes two files
under `scripts/`, and `categorisePath` calls an unrecognised path `both`.

The task ended with `review: null` and `verificationAttempts: 12`. `finish`
requires an independent APPROVE recorded for the verified tree, so the task
could not complete; no PR was ever opened and issue #2599 is still open.

The rounds had nowhere to land. Four review rounds over one change, and the
only record of the first three was a conversation that the next session could
not read. Round 2 opened with "the five previous findings are genuinely
addressed" — findings that existed only in the round-1 session's context.

## The attempt behind the gh2599 work

The gh2599 work is a circuit implementer attempt, and the attempt is the
reason it looks like it takes hours.

`%APPDATA%\com.alond.buildmesh\circuits\runs\run-718` holds three files, all
written at 02:43:03 on 27 October:

```
agent-6185.turn                          14 bytes
implementer-attempt1.prompt.md          1292 bytes
payload-696d706c656d656e746572-1.txt    157 bytes
```

There is no `implementer-attempt1.result.md`. The attempt's own contract says
it must write that file, ending with `BUILDMESH_HANDOFF_V1: READY` or
`BUILDMESH_HANDOFF_V1: BLOCKED`, only when it has completely finished. Its
brief names issue #2599 and the gh2599 worktree.

The mcode process for that attempt is still running: started 02:43:08, alive
9h20m later, 56 CPU-minutes consumed, 503 MB resident. It did real work — the
seven amends and four review rounds above are its work — and it never
terminated. `agent-6185.turn` still reads `implementer` / `1`: the run is on
attempt 1 and has never been retried or failed.

Two more attempts on the same host are in the same shape: run-717 (started
00:35, alive 11h30m) and run-719 (started 09:41, alive 3h20m), neither with a
result file.

`src-tauri/src/circuit/model.rs:198` already names the gap: "Process-level
enforcement (cancelling stuck spawns) is still a follow-up." The watchdog is
written for the case where an agent stops producing output. This attempt keeps
producing output and keeps working, so no wait budget expires and the gate
waits forever.

It is still holding the machine. A verification run in another worktree
reported `QUEUED agent-tests: waiting for an infrastructure-suite slot (limit 1;
held by agent-tests in
F:\src\buildmesh\.worktrees\gh2599-perffrontend-renderer-compositor-thread-pin)`.
The stuck attempt is not only slow: it is holding the slot every other agent's
verification needs.

## Fixed in this change

**A review-round ledger.** `npm run harness -- review` records a verdict, a
summary and findings against the active task and appends the round to
`.harness/reviews.jsonl`. `npm run harness -- review --show` returns the last
verdict, the findings still outstanding and the paths whose content changed
since the last round reviewed, so a follow-up reviewer starts from the recorded
state instead of re-reading the whole change. The review skill and the harness
reference name the command and the scoped check.

The completion guards are unchanged: the verdict lands in the same `task.review`
field `update --spec` writes, the reviewed code is bound by a content tree that
`finish` compares against the receipt's, and an approval that still carries
findings is refused.

## Filed

| Issue | Finding |
|---|---|
| [#2619](https://github.com/alondero/buildmesh/issues/2619) | 66 of the last 150 commits on `main` select the whole Rust graph. `scripts/*` (89) and `.github/*` (22) are the forcing paths; `src/types/generated/*` (42) is `both` by design. |
| [#2620](https://github.com/alondero/buildmesh/issues/2620) | 798 `Start-Sleep` calls (671 min) and 13,183 `task_output` polls. `harness wait` exists since #2104; nothing enforces it. |
| [#2621](https://github.com/alondero/buildmesh/issues/2621) | A repair loop re-runs scope-complete verification until the gates time out. The policy says not to; nothing enforces it. |
| [#2622](https://github.com/alondero/buildmesh/issues/2622) | A browser behaviour has no repo-owned scenario, so three separate review rounds each wrote their own throwaway harness. |
| [#2623](https://github.com/alondero/buildmesh/issues/2623) | 3,621 one-call-at-a-time `gh` round trips cost 18 hours of agent wall-clock. |
| [#2624](https://github.com/alondero/buildmesh/issues/2624) | A circuit spawn attempt that never writes its result file runs for hours while the gate waits. The gh2599 implementer attempt is one. |
| [#2626](https://github.com/alondero/buildmesh/issues/2626) | A Windows rename race in `managed_config::tests::concurrent_writes_do_not_share_a_temp_name` fails intermittently. Filed while verifying the review-ledger change, not by this retro's analysis. |
| [#2628](https://github.com/alondero/buildmesh/issues/2628) | The Rust wall-clock budget fails a verification on a machine running concurrent agents. The budget is the defect, not the symptom, so it was filed rather than suppressed. |

## Not covered

- The Rust and frontend test suites were not profiled. Their wall-clock is
  reported here as gate time, not as a defect.
- The mcode session store holds 2,541 non-Buildmesh sessions that were not
  analysed; the pattern may differ there.
- The gh2599 implementation itself was written in another harness's worktree.
  The mcode sessions analysed are the review rounds.
- The 15-minute `agent-tests` gate deadline, and whether it is the right budget
  on a loaded machine, is left to #2621.
