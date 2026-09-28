# September circuit completion audit

Investigated on 2026-09-28 against base
`e1a746fcb2433514ec0bb8c632a4898c757fbab0`, using read-only queries of the
stable profile ledger, retained logs, original transcripts, and a read-only
terminal WebSocket subscription. No live run outcomes were rewritten.

## Observed failures

| Mesh / runs | Evidence | Cause and correction |
|---|---|---|
| Buildmesh 247, Lambo 248/249, Pixelpath 254 | Feedback had `prompt_delivery=intent` and an uncertain prompt effect. Pixelpath's log recorded successful injection immediately followed by `commit failed: Query is not read-only`. | Recording prompt correlation advanced the history revision without updating the worker's fence. Return that revision to the worker; persist delivery acknowledgement independently so a later receipt or restart cannot lose it. Observation replays the acknowledgement, never the prompt. |
| Pixelpath 253 | The original reviewer message was 11,241 characters, with an explicit Request changes verdict near the end. Stored review output stopped at the 4,000-byte preview limit, before the verdict; classification returned blocked. | Circuit reports reused display-preview parsing. Pass an explicit text budget through the harness parsers: full text for circuit reports, bounded text for display. Preserve legacy revision matching for saved pre-input boundaries. |
| Pixelpath 252 / reviewer 4588 | No session identity; terminal retained the review prompt in its input box. Startup output showed `model: loading` when injection began; the log reported Enter acknowledged by subsequent output. | A process being alive and producing boot output does not prove its composer accepts submission. Codex now requires a loaded model header and input marker before the first PTY prompt; the wait is cancellable and preserves input ownership. |

Buildmesh 244 did complete after a review/fix round. Runs 241, 242, and 246
ended when reviewer nodes were closed, after reporting unavailable session
identities. Run 243 ended when its source was closed during the second fix
round. Those terminal closures are established; their earlier startup causes
cannot be proven from the retained node rows. They must not be relabelled as
successful runs or assumed to share every cause above.

Lambo 250/251 and Pixelpath 255 were waiting behind active runs occupying their
configured two-run capacities. This was downstream of the stalled runs, not
evidence that admission capacity itself was broken.

## Regression boundaries

- Real SQLite submission history and step transitions distinguish the old
  revision rejection from successful acknowledgement, while still rejecting
  cancellation and stale revisions.
- Prompt acknowledgement tests exercise transaction rollback, restart, late
  receipt history, cancellation, wrong attempts, and refusal to dispatch twice.
- File-backed and OpenCode-store report tests retain long reports, keep display
  previews bounded, and distinguish equal-length changes after byte 4,000.
- The Codex adapter checks loading, loaded, missing-input, and newer-loading
  output sequences. Original stable runs are evidence of the defects, not a
  live end-to-end validation of the changed binary.

## Recovery limits

The fixes prevent these paths in new work. Existing uncertain prompts without
a durable acknowledgement still need inspection and an operator-recorded
outcome; a log line is not silently promoted to a delivery receipt. Review
approval remains separate from delivery and foreground completion. Retained
source work can be reviewed again after installing the corrected build.
