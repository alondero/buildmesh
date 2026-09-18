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
- **With prefill:** `cline -i "<prompt>"` — CR/LF is flattened first, because
  `cmd.exe /c` on Windows treats a bare newline in a quoted argument as
  end-of-command.
- **Windows** resolves the npm `cline.cmd` shim through `cmd.exe`
  (`WindowsShell::Cmd`); **macOS / Linux** spawn the executable directly
  (`WindowsShell::Direct`).

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

Buildmesh's Anthropic surface emitter also sets `ANTHROPIC_AUTH_TOKEN` (and
`ANTHROPIC_BASE_URL` / `ANTHROPIC_MODEL` for a custom endpoint); the OpenAI
surface emitter sets `OPENAI_API_KEY` (plus `OPENAI_BASE_URL` / `OPENAI_MODEL`).
The injected values come from the attached account only — Cline keeps its own
authentication as the fallback.

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
- **npm shim vs direct binary.** The default Windows spawn goes through the npm
  `cline.cmd` shim (which runs Node, then the platform binary) because
  `CreateProcess` cannot execute a batch file directly. Buildmesh also detects
  a direct `node_modules\@cline\cli-windows-x64\bin\cline.exe` if the shim is
  not present. Spawning the binary directly avoids `cmd.exe` and Node but loses
  the wrapper's CA-certificate harvesting
  (`~/.cline/cli-node-extra-ca-certs.pem` → `NODE_EXTRA_CA_CERTS`).
- **`CLINE_BIN_PATH`.** Set this environment variable to an absolute path to
  have detection prefer a specific Cline executable. It wins over the npm shim
  and the `node_modules` walk when the path exists.
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
