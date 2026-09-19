---
name: cline-harness-capabilities
description: Capability contract and Native Provider boundary for the Cline CLI harness, including the Buildmesh Model Provider env-var seam
metadata:
  type: reference
  harness: cline
  verified_cli_version: 3.0.62
---

# Cline harness vs Buildmesh capability contract (2026-09-18)

Primary question: which Buildmesh harness capabilities does the Cline CLI
actually support, how is it spawned and resumed, and how do the user's
Buildmesh **Model Provider** accounts reach it without Buildmesh taking over
Cline's own authentication?

Verified against:

- Live `cline` 3.0.62 on Windows (npm shim at `%APPDATA%\npm\cline.cmd`),
  `cline --help` and `cline --version`
- Buildmesh adapter + trait: `src-tauri/src/agent/provider/adapters/cline.rs`,
  `provider/mod.rs`, `capabilities.rs`, `detection.rs`,
  `preferences/compatibility.rs`
- Related pages: [user guide](../user-guide.md),
  [troubleshooting](../troubleshooting.md)

## Executive answer

Cline is an interactive terminal agent that Buildmesh drives as a **Native
Provider**: Cline owns its own credentials (`cline auth`) and Buildmesh never
writes Cline's configuration. Buildmesh spawns `cline -i`, resumes with
`cline -i --id <id>`, and can forward a model, a reasoning effort, and a
prefill prompt. Attention-hook provisioning and transcript reading are **not**
wired yet, so those capabilities are advertised honestly as unsupported.

## What Buildmesh wants from a harness

| Flag / method | What Buildmesh uses it for | Cline |
|---|---|---|
| `supports_resume` | Enables the resume invocation and auto-resume on startup | Yes (`--id <id>`) |
| `auto_resume_on_startup` | Re-spawns suspended nodes that have a stored session id | Yes |
| `self_assigns_session_id` | Fresh spawn uses no mint flag; the id is captured later | Yes |
| `session_assign_args` | Fresh-spawn flags when Buildmesh mints the id | Empty (Cline self-assigns) |
| `resume_args` | Resume flags | `--id <id>` |
| `supports_model_override` | `--model <id>` from Mesh / app defaults | Yes |
| `effort_control` | Reasoning-effort vocabulary | `--thinking none\|low\|medium\|high\|xhigh` |
| `supports_prefill` | Seed the first turn from issue / handover text | Yes (`-i "<prompt>"`) |
| `supports_extra_args` | Verbatim circuit-author CLI flags | Yes |
| `requires_attention_hook` | Autopilot attention gate | No (hook not shipped) |
| `produces_readable_transcript` | Coordinator Node Digest / archive resume | No (reader not shipped) |
| `available_on` | Spawn Menu filtering | Windows, macOS, Linux |

## Cline's actual CLI

| Flag | Meaning | Buildmesh |
|---|---|---|
| `-i`, `--tui` | Open the interactive TUI | Baked into the base recipe |
| `<prompt>` | Positional prompt; seeds the first turn | Used as the prefill |
| `--id <session-id>` | Resume an existing session | Resume recipe |
| `-m`, `--model <model-id>` | Model for the session | Model override |
| `--thinking <level>` | `none\|low\|medium\|high\|xhigh` (bare flag = `medium`) | Effort override |
| `-P`, `--provider <id>` | Provider id (default `cline`) | Left at default |
| `-c`, `--cwd <path>` | Working directory | Provided by the spawn working directory |
| `--worktree`, `--kanban`, `-z`/`--zen`, `--team-name` | Cline's own orchestration surfaces | Never passed (duplicate Buildmesh's) |
| `--yolo` | Not present in 3.0.62 `--help` | Never passed |
| `--hooks-dir`, `CLINE_HOOKS_DIR` | Documented but inert in 3.0.62 | Never passed |
| `--data-dir` | Isolated local state; auto-enables Cline's sandbox | Never passed (`CLINE_DATA_DIR` stays a manual escape hatch) |

Session ids are self-assigned and shaped `<epochms>_<5 base36 chars>`
(for example `1789757012702_7of3e`). There is no flag that mints an id, so
Buildmesh cannot pin one at spawn, and live capture is handled by the
separate capture pipeline.

## Spawn recipe

- **Fresh:** `cline -i` — the TUI opens with no session id.
- **Resume:** `cline -i --id <id>` — the base `-i` survives composition.
- **With prefill:** `cline -i "<prompt>"`. Prefill normalisation is
  platform-aware: Windows flattens CR/LF/CRLF to single spaces (the
  `cmd.exe /c` end-of-command trap); macOS / Linux preserve the line
  structure and only normalise CRLF→LF (direct spawn, argv elements
  carry newlines safely). The platform-agnostic flattening that lived
  in the adapter for the first draft of this slice destroyed multi-line
  prompts on Unix; it now lives in
  `agent::launch::normalize_prefill_for_platform`.
- **Windows** wraps the spawn with `cmd.exe /c` (`WindowsShell::Cmd`),
  even when the resolved path is an absolute path off `PATH`.
  **macOS / Linux** spawn the executable directly
  (`WindowsShell::Direct`).
- **Off-PATH detection.** When Cline was detected via `CLINE_BIN_PATH`
  or the `node_modules\@cline\cli-windows-{x64,arm64}\bin` walk, the
  resolved absolute path is stored on the profile's `executable` field
  and threaded through `spawn_environment::wrap` as
  `executable_override`. `cmd.exe` resolves the npm shim directly via
  `cline.cmd` only when the npm prefix is on `PATH`; for everything
  else the orchestrator hands the absolute path to `cmd.exe /c`
  explicitly.

## Native Provider boundary and the env-var seam

Cline runs as a **Native Provider**:

- Buildmesh **never** writes `~/.cline/data/settings/providers.json` and never
  invokes `cline auth`. Users add providers through Cline's own flow.
- Buildmesh injects the API key from a **Model Provider** account the user has
  already attached, at spawn time, as environment variables on the Cline
  process. No Buildmesh-side pairing is required for Cline itself: when no
  `cline`-specific pairing exists, Buildmesh falls back to any pairing for that
  account whose API surface Cline speaks (Anthropic or OpenAI). Attach the
  account once under Claude Code (Anthropic surface) or Codex (OpenAI surface)
  and `cline` inherits it.
- The canonical env-var set Cline reads for a given provider, and the full
  provider catalogue, are owned by Cline. Defer to `cline auth` for anything
  not listed here:

| Provider | Environment variable |
|---|---|
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| DeepSeek | `DEEPSEEK_API_KEY` |

> **Important consumer-aware branch (issue #1773 review).** The
> Anthropic surface emitter that targets **Claude Code** deliberately
> emits `ANTHROPIC_AUTH_TOKEN=<key>` and blanks `ANTHROPIC_API_KEY=""` for
> custom endpoints — that's the OpenRouter trap that forces Claude Code
> through the third-party token instead of a shell-set Anthropic key.
> Cline does **not** read `ANTHROPIC_AUTH_TOKEN`. If we naively fed
> that emitter to a Cline spawn, the Cline process would see
> `ANTHROPIC_API_KEY=""` and fail to authenticate. The env-builder
> therefore branches on the consumer harness: a `cline:<account>` spawn
> gets a Cline-shaped emitter (`ANTHROPIC_API_KEY=<key>` non-empty,
> `ANTHROPIC_BASE_URL=<base>` when set, `ANTHROPIC_MODEL=<primary>` when
> configured) — no `ANTHROPIC_AUTH_TOKEN`, no key-blanking. The
> OpenAI surface emitter is already the right shape for Cline so it
> is reused as-is.
>
> Regression-pinned by
> `preferences::compatibility::tests::cline_anthropic_default_endpoint_sets_anthropic_api_key`,
> `…_custom_endpoint_sets_anthropic_api_key_not_blank`, `…_openai_custom_endpoint_sets_openai_api_key`,
> and the Claude-Code contract pinned by
> `…_claude_anthropic_custom_endpoint_still_uses_auth_token_trap`.

## State and isolation

Cline uses one shared global `~/.cline`. There is no per-node `--data-dir`, so
the capture/resume invariant reduces to "same `--cwd`". `CLINE_DATA_DIR`
remains available as a manual per-Mesh escape hatch for a Mesh that explicitly
demands isolation, but Buildmesh does not set it. Be aware that concurrent
Cline processes share the same local session database and hub daemon; if you
run many Cline nodes at once, watch for lock contention and hub port reuse.

## Troubleshooting

- **Windows Application Control / antivirus blocks `cline.exe`.** Cline's own
  diagnostic asks you to run the binary path directly and check
  `Get-AuthenticodeSignature`. Buildmesh does **not** auto-unblock a blocked
  binary — resolve the block with your organisation's tooling, then restart
  Buildmesh so detection refreshes.
- **npm shim vs direct binary.** When the npm prefix (`%APPDATA%\npm`) is
  on `PATH`, Windows spawns through `cmd.exe /c cline`, which resolves
  the `cline.cmd` shim, which runs Node, which loads the platform
  binary. When Cline was detected via `CLINE_BIN_PATH` or the
  `node_modules\@cline\cli-windows-{x64,arm64}\bin` walk, the
  orchestrator hands the resolved absolute path to `cmd.exe /c` so the
  same `cmd.exe`-wrapped spawn shape is preserved. The CA-cert
  harvesting the npm shim does (`~/.cline/cli-node-extra-ca-certs.pem`
  → `NODE_EXTRA_CA_CERTS`) only fires on the npm-shim path; if you
  use the resolved-direct path you opt out of that wrapper. (Both
  architectures — `x64` and `arm64` — are probed because `@cline/cli`
  ships separate platform-specific packages.)
- **`CLINE_BIN_PATH`.** Set this environment variable to an absolute path to
  have detection prefer a specific Cline executable. The resolver walks
  `CLINE_BIN_PATH`, then the npm shim, then
  `node_modules\@cline\cli-windows-x64\bin`, then
  `node_modules\@cline\cli-windows-arm64\bin`. The first one that exists
  wins, and its absolute path is propagated to the spawn command
  through the profile's `executable` field.
- **WSL.** Cline in WSL is not a supported or tested runtime in this release.
  The guest-side and Windows-from-WSL detection probes deliberately skip
  Cline, so a WSL-only install does not produce a Spawn Menu row.
- **A Cline row does not appear.** Confirm `cline` (or `cline.cmd`) is on
  PATH, or that `~/.cline` exists, then restart Buildmesh. Detection runs once
  at startup.

## Sources

- `cline --help` / `cline --version` (3.0.62)
- Buildmesh `agent::provider::adapters::cline` adapter and its unit tests
- Buildmesh `agent::detection` (install resolver order) and
  `preferences::compatibility` (`resolve_pairing` surface fallback)
