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
Status: current against `devin 3000.14.30821 (a09cd7f323ee)` on 2026-11-24.

Devin CLI is Cognition's native interactive terminal coding agent (`devin` /
`devin.exe`). Buildmesh launches the native binary directly, passes interactive
prefill via `-- <prompt>`, provisions attention hooks into
`.devin/config.local.json`, and reads usage directly from the Windsurf / Codeium
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
  everywhere (`WindowsShell::Direct`). Windows GUI discovery includes
  `%LOCALAPPDATA%\devin\cli\bin` in `supplemental_user_bin_dirs`, and Unix
  discovery includes `~/.local/bin`. Config directories are not used for
  detection because `~/.config/devin` can exist without the CLI binary.
- **Prefill & model override**: `supports_prefill: true`,
  `prefill_auto_submits: true` (`-- <prompt>`), `supports_model_override: true`
  (`--model <MODEL>`). Effort override is `EffortControlKind::None` (thinking
  level is interactive-only).
- **Permission modes**:
  - `unattended` (default; `AttentionLaunchMode::SkipPermissions` reflects this default posture): `--permission-mode dangerous --respect-workspace-trust false`
  - `prompt`: no permission flag (Devin asks for approval and can emit `PermissionRequest` hooks)
- **Session identity & resume**: `self_assigns_session_id() -> true`,
  `captures_session_id_from_pty() -> false`, `supports_resume: true`
  (`--resume <SESSION_ID>`). `session_id` is captured from attention hook
  payloads (`http::routes::attention::normalizers::devin`, UUID format per
  `Hooks Overview #command-hooks`).
- **Attention hooks**: provisioned into `.devin/config.local.json` under the
  `"hooks"` key (documented as a gitignored local override in
  `Hooks Overview #where-hooks-live`) for `SessionStart`, `UserPromptSubmit`,
  `Stop`, `PermissionRequest`, `PreToolUse`, and `PostToolUse` (the tool hooks
  match `^(ask_user_question|exit_plan_mode)$`). `http::routes::attention::normalizers::devin`
  parses and classifies these payloads (`tests/fixtures/attention/devin.json`).
  `circuit_observation` stays `ObservationStrategy::UNWIRED` until end-to-end
  hook delivery is live-verified.
- **Background inference**: `None` (`background_recipe` stays unwired until
  `-p/--print` is live-verified with an authenticated session).

## Usage meter contract

- **Credentials**: Windows `%APPDATA%\devin\credentials.toml`, macOS/Linux
  `$XDG_DATA_HOME/devin/credentials.toml` or
  `~/.local/share/devin/credentials.toml` (`windsurf_api_key`, optional
  `api_server_url` defaulting to `https://server.codeium.com`), with fallback to
  `WINDSURF_API_KEY` and `WINDSURF_API_SERVER_URL` override.
- **Request**: `POST {api_server_url}/exa.seat_management_pb.SeatManagementService/GetUserStatus`
  with `Content-Type: application/json`, `Connect-Protocol-Version: 1`, and
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
