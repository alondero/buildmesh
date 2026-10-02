# Circuit run 293: Codex context messages

Investigated on 2026-10-02 against base
`5c81d05c8e5b60142f351d3fdf33fbf5ae0dd831`. Evidence came from read-only
inspection of the stable profile ledger, application logs and Codex rollout.

## Observed checkpoint

Run 293 on the Nestlin mesh (68) borrowed source agent 4767. Its `await_source`
step became Unverified with `ReportUnavailable`: the transcript contains an
unrecognised or malformed record. No reviewer had started.

The source was Ready, with session
`01a0fba8-4303-7091-8e5e-a0295f8c119a` and process incarnation
`1790928500150`. The rollout published a final answer linking Nestlin PR 327
at 09:08:35.596 UTC and a matching `task_complete` receipt at 09:08:35.632 UTC.
The circuit ledger retained that report and foreground completion. The Ready
projection followed at 09:08:41.557 UTC.

## Root cause

Two `response_item` messages with `role: developer` appeared at 09:08:24.381 UTC,
carrying model-switch and collaboration-mode context. The Codex dialogue parser
marked every message role other than user or assistant as malformed. The
report reader rejects any malformed record in its bounded 256 KiB window,
so these valid context messages poisoned an otherwise completed turn.

The worker does keep probing Unverified report gates. It rereads the same
unchanged transcript, however, so automatic retries cannot repair this parser
mismatch. The malformed-record error also deliberately prevents native-receipt
fallback: a receipt cannot safely override positive evidence of unreadable
transcript content. Weakening that fallback would hide the parser defect.

## Corrective behavior

The Codex dialogue adapter recognizes system and developer messages as context,
without adding turns or marking them malformed. Missing and unknown roles still
produce a malformed-record error. The native completion reader remains
conservative: context or other activity after completion invalidates the old
receipt until a newer current turn completes.

A regression replays the model-switch context, final report and native receipt
through the production report reader, readiness preflight and review stepper.
It starts from the same report-unavailable checkpoint and checks that review
can be scheduled, the blocker clears, and report handoff does not fabricate
complete owned-work verification. Negative cases retain rejection of unknown
roles and post-completion context changes.

The original bounded live transcript was replayed locally: it returned
`MalformedRecord`, while removing only the two developer records admitted the
same final report and completion timestamp. After the fix, the unchanged
transcript and captured run context passed readiness with the original session,
incarnation and input stamp, cleared the report blocker and made the reviewer
runnable in the stepper. This replay dispatched no external effects; its
private artifacts and temporary test are not part of the committed regression.

The live stable run was not rewritten or attested complete. An updated running
application is required before this source change can affect it. Automatic
handoff requires the source process, session and input boundary to remain
current. An update that restarts the source may introduce a missing-process or
new-incarnation checkpoint; the old report cannot bypass those fences. See
[troubleshooting](../troubleshooting.md#a-completed-codex-agent-is-waiting-for-a-usable-harness-report)
for the recovery procedure.

## Verification

The portable context-message regression failed with `MalformedRecord` before
the fix. Afterward, all 28 report tests passed, including the temporary live
replay. The final full serial Windows Rust check passed 4,015 library tests
and 18 integration tests, with 27 library tests and one doctest ignored.
The private replay test had been removed before that final run.

All-target Clippy passed with four library and 36 test warnings, matching the
diagnostic set from an isolated copy of the recorded base. Neither changed file
produced a warning. ESLint, 33 documentation tests, the documentation contract
and agent checks passed. Generated TypeScript bindings did not change. The
initial full-check invocation stopped before tests on a Node color-environment
warning; clearing `FORCE_COLOR` in the check process allowed the passing rerun.
