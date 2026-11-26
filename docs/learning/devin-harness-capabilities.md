---
name: devin-harness-capabilities
description: Capability contract and usage meter for the Devin CLI harness
metadata:
  type: reference
  harness: devin
  verified_cli_version: 3000.14.30821
---

# Devin harness and usage meter

Audience: maintainers changing the Devin adapter or its usage meter.
Status: current against `devin 3000.14.30821 (a09cd7f323ee)` on 2026-11-25.

Devin CLI is Cognition's native interactive terminal coding agent (`devin` /
`devin.exe`). Buildmesh launches the native binary directly, passes interactive
prefill via `-- <prompt>`, and reads usage directly from the Windsurf / Codeium
`GetUserStatus` ConnectRPC endpoint using the CLI's saved `credentials.toml`.

## Sources

Verified against:

- Installed Windows binary `devin 3000.14.30821 (a09cd7f323ee)` at
  `%LOCALAPPDATA%\devin\cli\bin\devin.exe` (`devin --help`, `devin auth status`,
  and binary inspection of `chisel-api` and `/usage`).
- Official documentation: [Devin CLI](https://devin.ai/cli),
  [Essential Commands](https://docs.devin.ai/cli/essential-commands),
  [Lifecycle Hooks](https://docs.devin.ai/cli/extensibility/hooks/lifecycle-hooks),
  and [Hooks Overview](https://docs.devin.ai/cli/extensibility/hooks/overview)
  (including `#where-hooks-live` and `#command-hooks`).
- Official brand mark: [Devin SVG favicon](https://devin.ai/favicon.svg).

## Harness capability contract

- **Binary & detection**: `devin` (`.exe` on Windows), native compiled binary
  everywhere (`WindowsShell::Direct`). Windows GUI discovery reads
  `LOCALAPPDATA` (`%LOCALAPPDATA%\devin\cli\bin`, falling back to
  `<home>\AppData\Local\devin\cli\bin`), and Unix discovery includes
  `~/.local/bin`. Config directories are not used for detection because
  `~/.config/devin` can exist without the CLI binary.
- **Prefill & model override**: `supports_prefill: true`,
  `prefill_auto_submits: true` (`-- <prompt>`), `supports_model_override: true`
  (`--model <MODEL>`). Effort override is `EffortControlKind::None` because
  there is no independent `--effort` argument. The CLI enumerates
  effort-qualified model variants through `devin models list --format json`,
  but that output is the global catalogue rather than the entitlement-filtered
  account selection. Buildmesh therefore does not advertise Devin model/effort
  discovery; users can still enter an exact effort-qualified variant manually
  as `--model` when their account offers it.
- **Permission modes**:
  - `unattended` (default): `--permission-mode dangerous --respect-workspace-trust false`
  - `prompt`: no permission flag (Devin asks for approval)
- **Session identity & resume**: `self_assigns_session_id() -> true`,
  `captures_session_id_from_pty() -> false`, `supports_resume: true`
  (`--resume <SESSION_ID>`). Live-verified on `3000.14.30821`: `session_id`
  is a lowercase `<word>-<word>` slug (`phantom-jellyfish`,
  `tangy-peony`, `exciting-ghost`, `spangled-click`; issue #2864's
  independent capture saw `curved-gravity`, `good-appliance`) — the docs'
  `3f8d1c2a-...` example is the payload's `prompt_id`, not `session_id`,
  and `resume` accepts a session id *or name*. `devin --resume <slug>`
  fires `SessionStart` with `source: "resume"`. Capture reads the CLI's
  own session store — `cli/sessions.db` under `%APPDATA%\devin` (Roaming)
  on Windows — filtering the `sessions` table by `working_directory`; the
  `services::devin_session` poller wires the row's `id` column into
  `cli_session_id`.
- **Attention hooks**: wired since issue #2862. Live capture against a print
  session with `.devin/hooks.v1.json` provisioned confirmed:
  - `hook_event_name` is present on `SessionStart`, `UserPromptSubmit`,
    `PreToolUse`, `PostToolUse`, `PermissionRequest`, `Stop` (and `SessionEnd`).
  - `session_id` is on every event; `prompt_id` (a UUID) is on every event
    once a turn has started and absent on `SessionStart`, matching the docs.
  - `UserPromptSubmit` adds `prompt` and `source`; `Stop` adds
    `stop_hook_active` and `last_assistant_message`; tool events add
    `tool_name`, `tool_input` and `tool_use_id` (`call_<hex>#<hex>` or
    `<tool>:<n>#<hex>`); `PostToolUse` adds `tool_response` (`success`,
    `output`, `error`). `ask_user_question`'s `tool_input` is Claude's
    `questions[].options[].label` shape.
  - `SessionEnd` fires for print-mode sessions (`reason: "other"`), sometimes
    after the session output completes. `PostToolUse` does not fire for
    `ask_user_question` in print mode because the interactive tool never
    completes there. `exit_plan_mode` is a documented core tool but does not
    exist in print mode, so its payload shape is unverified (uniform envelope
    assumed).
  - On Windows `devin.exe` runs command hooks through `bash.exe -c` — on the
    verification machine an MSYS2 bash resolved from PATH — as a descendant
    of the agent process (`devin.exe -p` -> `devin.exe acp` -> `bash.exe` ->
    hook), so POSIX syntax (`$VAR`, `>/dev/null`, `|| true`) is required and
    cmd.exe syntax would break, and per-agent environment variables propagate
    to the hook shell (verified with `BUILDMESH_PORT`/`BUILDMESH_SESSION_ID`).
    What devin does on a Windows machine with no `bash.exe` on PATH is
    untested.
- **Interactive submit lifecycle**: live probes on 2026-11-25 showed that
  input written about 50ms after spawn is silently discarded before the
  composer attaches, while input written after half a second lands. The
  successful writes also paced the Enter after the prompt rather than sending
  `prompt+Enter` in one write; the same live probes showed the concatenated
  shape only stages the text in Devin's composer. Buildmesh therefore waits
  for the composer marker (`Ask Devin to build features`) before typing an
  initial prompt and does not use the generic concatenated unconfirmed-submit
  replay. If no `UserPromptSubmit` hook confirms a submit, the existing
  bounded confirmation window reverts the node to Ready instead of treating
  the unacknowledged input as running work.
  The marker is pinned copy in `3000.14.30821`; if a CLI update changes it,
  initial delivery degrades to the existing bounded blind fallback rather
  than waiting forever. Landed interactive submits do fire
  `UserPromptSubmit` with the verbatim `prompt`, a UUID `prompt_id`, and the
  word-slug `session_id` (captured sessions included `lilac-chameleon`,
  `upbeat-limburger`, `evanescent-fireplace`, `south-zenobia`,
  `innate-card`, and `heather-yellowhorn`).
- **Circuit completion**: live verification on 2026-11-25 ran a Devin
  reviewer step through the dev app. The staged prompt reached Devin through
  the interactive terminal, `UserPromptSubmit` and `Stop` hooks resumed and
  completed the node's lifecycle, Devin wrote the contracted UTF-8 result file,
  and the review circuit classified the report, routed `APPROVE`, and completed.
  A borrowed Devin implementation agent can have a finished turn from before
  the Circuit attached, with no result file owed. The Circuit reads the current
  main-chain message from Devin's local `sessions.db` for that report and
  rechecks the message before acting on it. New input or tool activity on the
  main chain invalidates the earlier report. Attention supplies the lifecycle
  signal. For a contracted turn, the result-file fallback uses the current
  input and lifecycle timestamps even when Devin acknowledges the file in a
  later assistant message.
- **`.claude/` hook import (verified, issue #2864)**: a `devin -p` probe on
  3000.14.30821 fired `SessionStart`, `UserPromptSubmit` and `Stop` hooks
  defined in `.claude/settings.local.json` — `read_config_from.claude` is on
  by default and the hook shell is POSIX (`$VAR` expands, `%VAR%` does not).
  Payloads are Claude-shaped (`hook_event_name`, `prompt_id` UUID), but the
  `session_id` is a word slug (`curved-gravity`, `good-appliance`), not a
  UUID. The provisioned `.claude` command posts only when `BUILDMESH_HARNESS`
  is unset or `anthropic`, so imported Claude hooks cannot produce
  cross-harness callbacks for a devin turn. Disabling
  `read_config_from.claude` was rejected as the fix: it gates all Claude
  imports (rules, skills, MCP servers), not just hooks.

- **Background inference**: wired. Live verification on 2026-11-25 ran
  `devin --print --respect-workspace-trust false --process-model in-process
  --permission-mode auto --verbosity 0 --prompt-file prompt.txt` from an
  authenticated empty temporary directory outside any repository, matching the
  product's classifier and naming working directories. A simple exact-output
  prompt exited 0 after 5.844 seconds with the requested answer on stdout; a
  representative circuit-classification prompt exited 0 after 5.140 seconds
  with `COMPLETED`; adding `--model gpt-6-luna-low` exited 0 after 2.984
  seconds. Evidence: `.tmp/live-probes/background/simple-external-empty.json`,
  `classifier-external-empty.json`, and `simple-external-with-model.json`.
  `auto` permission mode auto-approves only read-only tools; print mode rejects
  confirmation-required shell or side-effecting tools instead of asking. That
  is intentional for self-contained classification and naming prompts. Probe
  variants run inside this repository are not valid contract evidence because
  Devin correctly loads repository agent instructions and can continue working
  after the first answer; their exit-143 stderr artifacts are probe timeout
  terminations, not CLI exit statuses. The adapter uses the prompt-file
  transport and stdout result channel, with no provider routing or advertised
  prompt-size limit.

## Usage meter contract

- **Credentials**: Windows `%APPDATA%\devin\credentials.toml`, macOS/Linux
  `$XDG_DATA_HOME/devin/credentials.toml` or
  `~/.local/share/devin/credentials.toml` (`windsurf_api_key`, optional
  `api_server_url` defaulting to `https://server.codeium.com`), with fallback to
  `WINDSURF_API_KEY` and `WINDSURF_API_SERVER_URL` override.
- **Request**: `POST {api_server_url}/exa.seat_management_pb.SeatManagementService/GetUserStatus`
  over `https` (or `http` for loopback addresses only) with
  `Content-Type: application/json`, `Connect-Protocol-Version: 1`, and
  `metadata.api_key`. Live verification on 2026-11-25 against a signed-in Pro
  account returned `BILLING_STRATEGY_QUOTA`, daily and weekly quota windows at
  100% remaining with Unix reset timestamps, and a positive
  `overageBalanceMicros` balance. The ACU/dollar billing-cycle and meaningful
  legacy prompt-credit branches are covered by adapter tests because this
  account's quota response carries no billing-cycle consumption fields and only
  `-1` prompt-credit sentinels.
- **Rendering**:
  - Daily and weekly quota windows (`100 - remainingPercent`, clamped to `0..=100`,
    with Unix reset timestamps) unless `hideDailyQuota` / `hideWeeklyQuota` is set.
  - Positive `overageBalanceMicros` as a `USD` `BillingBalance`.
  - When quota windows are absent, ACU or dollar billing-cycle meters
    (`acuConsumed`/`acuLimit` or `dollarsConsumed`/`dollarsLimit`) and legacy
    prompt-credit percentage fallback.
- **Usage page**: [https://app.devin.ai/settings/usage](https://app.devin.ai/settings/usage)
  (`View usage`).
