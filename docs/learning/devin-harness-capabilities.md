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
  (`--model <MODEL>`). Effort override is `EffortControlKind::None` (thinking
  level is interactive-only).
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
- **Circuit completion**: `circuit_observation` stays
  `ObservationStrategy::UNWIRED` until a live Circuit run proves the hook
  surface end-to-end; Circuits complete turns via result-file handoff.
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

- **Background inference**: `None` (`background_recipe` stays unwired until
  `-p/--print` is live-verified with an authenticated session).

## Usage meter contract

- **Credentials**: Windows `%APPDATA%\devin\credentials.toml`, macOS/Linux
  `$XDG_DATA_HOME/devin/credentials.toml` or
  `~/.local/share/devin/credentials.toml` (`windsurf_api_key`, optional
  `api_server_url` defaulting to `https://server.codeium.com`), with fallback to
  `WINDSURF_API_KEY` and `WINDSURF_API_SERVER_URL` override.
- **Request**: `POST {api_server_url}/exa.seat_management_pb.SeatManagementService/GetUserStatus`
  over `https` (or `http` for loopback addresses only) with
  `Content-Type: application/json`, `Connect-Protocol-Version: 1`, and
  `metadata.api_key`.
- **Rendering**:
  - Daily and weekly quota windows (`100 - remainingPercent`, clamped to `0..=100`,
    with Unix reset timestamps) unless `hideDailyQuota` / `hideWeeklyQuota` is set.
  - Positive `overageBalanceMicros` as a `USD` `BillingBalance`.
  - When quota windows are absent, ACU or dollar billing-cycle meters
    (`acuConsumed`/`acuLimit` or `dollarsConsumed`/`dollarsLimit`) and legacy
    prompt-credit percentage fallback.
- **Usage page**: [https://app.devin.ai/settings/usage](https://app.devin.ai/settings/usage)
  (`View usage`).
