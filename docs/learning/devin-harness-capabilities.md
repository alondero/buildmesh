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
  (`--resume <SESSION_ID>`). `Hooks Overview #command-hooks` shows a truncated
  UUID-shaped `session_id` example (`3f8d1c2a-...`), though the full format is
  not stated in the docs and has not yet been confirmed from a live session.
- **Attention hooks & Circuit completion**: `attention_capability` is currently
  `None` (`requires_attention_hook: false`) and `circuit_observation` is
  `ObservationStrategy::UNWIRED`. Inside Autopilot Circuits, Devin completes
  turns via result-file handoff until live hook delivery, the Windows hook
  shell, the live `session_id` format, and `.claude/settings.local.json`
  deduplication (`Hooks Overview #where-hooks-live` notes Devin reads `.claude/`
  hooks when `read_config_from.claude` is enabled) are verified.
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
