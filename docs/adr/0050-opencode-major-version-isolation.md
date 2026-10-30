# ADR-0050: Isolate OpenCode major versions and node configuration

Status: accepted for the additive V2 adapter; V1 retirement remains gated.
**Date:** 2026-10-30
**Issue:** [#2655](https://github.com/alondero/buildmesh/issues/2655)

## Context

OpenCode 1 and 2 use the same executable name but incompatible plugins,
storage tables and TUI flags. A binary-name match cannot establish the harness
contract. V2's `plugins` and `permissions` keys also make V1 reject a shared
project config, so installing a V2 plugin in that file would break coexistence.

## Decision

Keep distinct `opencode` and `opencode2` identities. The initial detection
sweep performs no version subprocess. A deferred native worker probes each
resolved executable with a thirty-second deadline, including saved executable
pins, and applies the same compatibility rule as launch validation. Results
are cached by path, modification time and size; unknown results expire after
five seconds. An unknown V1 result is admitted for discovery and launch;
only a positively identified mismatched major is refused. V2 requires a known
matching major for both discovery and launch. Cross-runtime detection is
withheld until its version gate is validated. Interactive and background
preparation both use this cache.
The deferred worker merges only profile entries unchanged since its observation;
a user pin edited during a probe wins over that stale result.
After publishing the detection epoch, completion notifies open provider menus;
the frontend invalidates both its IPC promise and shared menu snapshot.

The unknown-version fallback preserves V1 compatibility but does not prove
that the executable is V1. If a real V2 binary's probe fails, Buildmesh can
offer and launch that binary through the V1 harness, with V1's attention
plugin and session capture. A successfully parsed probe result is needed to
distinguish the majors; V2 admission remains strict.

V2 always uses `--standalone`. Buildmesh assigns a `ses_` identity and persists
it before launch. A bounded `api --standalone` invocation prepares the session
model or forks the parent; the returned fork identity is persisted before
command construction. The full TUI receives neither `--model` nor `--fork`.
Background `run` retains its own model flag and JSON-lines decoder.
Ordinary resume looks up the persisted identity first. Only a structured
`SessionNotFoundError` triggers creation with that same id; other failures
remain errors. Regenerate never creates a missing parent to conceal a failed fork.

Select `.opencode/buildmesh2/<node-id>/opencode.json` through the child process's
`OPENCODE_CONFIG`. Its plugin entry names the managed package directory and
maps the assigned CLI session to the Buildmesh node and port. These V2-only
keys are never written into a shared project config. Existing project
JSON/JSONC and plugin arrays remain under OpenCode's own merge rules.

Configure an explicit `external_directory` deny in the V2 managed override. V2's allow
default otherwise widens filesystem access on upgrade. Background inference
also configures tool denies through project permissions and a `tool.use` policy.
Actual out-of-project reads or writes have not been tested against these rules
with the production interactive or background argv, including interactive
`--auto`. That enforcement remains an acceptance gate. These are CLI rules;
they do not replace the Mesh OS sandbox.

## Alternatives

Leaving V2 undetected would require every user to pin an executable. Probing
inside synchronous setup would add CLI startup latency to the window's startup
path. Reusing the V1 id would misinterpret existing transcripts and resumes.
Using V2 project `model` as an override does not apply the model in the observed
2.2.3 runtime. Sharing project V2 config with V1 is rejected by V1's validator.

## Consequences

V1 rows, capture services and readers remain intact. Default harness choice and
menu preferences are not migrated. Remove V1 only after the issue's live
Windows/macOS acceptance and a migration of eligible persisted rows are proven.
V2 currently refuses the Mesh OS sandbox because its prelaunch API cannot yet
share the PTY containment. This refusal happens before any project plugin loads.
V2 requires native launch preparation, disables extra arguments that
could bypass isolation, and advertises no Circuit foreground or child-work
contract. Managed files contain local callback identity and should remain local
to the checkout. See [runtime evidence](../research/opencode-v2-readiness.md).
