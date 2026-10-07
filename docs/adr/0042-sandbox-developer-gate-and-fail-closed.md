# 42. Gate the agent sandbox behind a developer flag and fail closed

Status: accepted (issue #2034; narrows what [ADR-0012](0012-windows-appcontainer-agent-sandbox.md) and the [ADR-0014 sandbox pivot](0014-pivot-windows-sandbox-off-appcontainer.md) shipped as a user-facing feature)

The per-Mesh `sandbox` preference must **not** be honoured in a shipped build.
Confinement is incomplete on every platform we ship, so the feature moves behind
`BUILDMESH_SANDBOX=1`, and a requested macOS sandbox that cannot be set up must
**refuse the spawn** instead of falling back to an unsandboxed launch.

## Context

ADR-0012 and the ADR-0014 pivot built an opt-in process sandbox and shipped it
with a per-Mesh checkbox in Project Settings. Two things make that unshippable as
a promoted feature:

1. **The guarantee is platform-shaped, and the copy was not.** The README opened
   by saying the toggle "confines every agent node … to its own Git worktree"
   and only later admitted that Windows does not confine files. Windows denies
   no filesystem read or write (#542 — a same-user restricted token cannot
   separate user files from the user-keyed kernel objects MSYS `bash` needs), Linux
   has no backend at all (#828), and a host sandbox cannot contain a WSL guest
   process. The honest behaviour and the advertised behaviour were different
   things.

2. **macOS failed open.** `spawn_environment::wrap` wrote the Seatbelt profile
   during command assembly; on a write failure it logged an error and returned a
   *direct* command. A user who asked for a sandbox got an unsandboxed agent with
   no visible signal beyond a log line — the one platform where the sandbox
   actually confines something silently stopped confining it.

A flag in a database is not a feature gate. `meshes.sandbox` persists, so any
gate applied only in the UI would leave a value written by a dev build quietly
confining (or, on macOS, silently not confining) released agents.

## Decision

**1. The gate is an environment variable, and it is authoritative in Rust.**
`BUILDMESH_SANDBOX=1` (only the exact value `1`; `true`/`0`/empty do not open it).
`sandbox::dev_sandbox_enabled()` is checked *before* the persisted column
everywhere the flag is consumed, via `sandbox::sandbox_requested`. A release
therefore ignores `meshes.sandbox` entirely: a value left on by a developer build
cannot start confining a released user.

This mirrors the existing `BUILDMESH_DISABLE_CRASH_WATCHDOG` convention, and
unlike a persisted "Developer mode" preference it needs no preference schema, no
migration, and no new shipped UI surface — there is no state a user can reach
that turns confinement on.

**2. The UI command is presentation only.** `sandbox_dev_mode_enabled` decides
whether the Mesh Settings checkbox renders. It is deliberately *not* the
guarantee: the spawn path re-derives the decision from the env var. Hiding the
control must never be the thing that keeps a release unsandboxed, because the
control can be hidden by a stale bundle or a failed read while the backend still
honours the flag.

**3. A requested sandbox fails closed.** `spawn_environment::wrap` returns
`Result<CommandBuilder, String>`; a Seatbelt profile-write failure is an `Err`
that `launch_process` surfaces as a `provider-error` before any PTY is opened, so
no child agent process is created. The message names the session, the cause, both
ways out (`BUILDMESH_SANDBOX=1` to retry, or turn the toggle off to run
unsandboxed) — a bare "spawn failed" would read as a broken harness.

**4. The column, the command, and the schema stay.** No migration removes
`meshes.sandbox`. The value remains readable and writable so the feature can come
back by deleting the gate, and a stored `true` in a release is inert rather than
an error.

**5. Copy states per-platform behaviour.** README, user guide, `CONTEXT.md`, and
the Settings help text describe what each launch target actually gets. macOS
fails closed; Windows restricts the process but does not deny file access; Linux
is inert; WSL is not contained.

## Alternatives considered

- **Persist a "Developer mode" preference in App Settings.** Discoverable without
  editing launch commands, but it ships a new user-facing preference plus UI
  surface to everyone, needs a preferences change, and creates a stored value
  that could be flipped in a release. Rejected: the env var has strictly less
  release risk.
- **Remove the feature entirely until #542/#828 land.** Cleanest, but discards a
  working macOS implementation and its profile tests; the code stays useful for
  the follow-up work. Rejected in favour of gating.
- **Keep the fail-open, document it as a dev-only limitation.** Smaller diff, and
  the fail-open stops being a shipping risk once the gate exists. Rejected
  because #2034 requires the fail-closed behaviour and it is the difference
  between a sandbox that is *absent* and one that is *broken*.
- **Gate only in the UI, leave the backend honouring the column.** Rejected: this
  is the stale-flag failure mode described above.

## Consequences

- **A shipped build confines nothing**, by construction rather than by default.
  The per-Mesh toggle disappears from Project Settings.
- **`spawn_environment::wrap` is fallible**, so its ~20 test call sites unwrap.
  `build_spawn_command` / `build_spawn_command_prepared` return `Result` too.
- **A macOS developer with a broken temp dir gets a refused spawn** rather than an
  unconfined agent. That is the intended trade.
- **The macOS fail-closed branch is `cfg`-gated**, so the end-to-end assertion
  only runs on a Mac. The portable half — a profile-write failure yields `Err`,
  and the message is actionable — is pinned on every host by
  `agent::sandbox::tests::seatbelt_command_reports_a_failed_profile_write_instead_of_a_command`.
- **Removing the gate is a one-line change** plus the checkbox's condition, once
  the platform gaps close.

## Verification

- `sandbox::tests::persisted_flag_is_inert_without_the_developer_gate` — the
  release guarantee: `sandbox = true` with the gate closed sandboxes nothing.
- `sandbox::tests::a_non_one_value_does_not_open_the_gate` — a guessed variable
  name (`true`, `0`, `yes`) does not opt a release in.
- `sandbox::tests::request_predicate_is_platform_agnostic` — guards the macOS
  branch from re-acquiring the fail-open via a stray `cfg!`.
- `agent::sandbox::tests::seatbelt_command_reports_a_failed_profile_write_instead_of_a_command` —
  a blocked profile path yields `Err`, never a launchable command.
- `spawn_environment::tests::requested_sandbox_without_a_writable_profile_yields_no_command`
  (macOS) — no command at all, so `spawn_child` is never reached.
- `project-settings-tab.test.tsx` — the toggle is absent by default (release) and
  present only when the gate reports open.

## Links

- Issue #2034 (launch gate), #542 (Windows read confinement), #828 (Linux
  backend), #497 (macOS Seatbelt), #528 (restricted token).
- [README — Agent sandboxing](../../README.md#agent-sandboxing-security-experimental),
  [user guide](../user-guide.md).