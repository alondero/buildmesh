# Smoke test: Windows-native direct-spawn (MiniMax + Kimi)

Manual runtime verification for the Windows-native `claude.exe` → ConPTY
spawn path. Out of scope for CI: requires real `claude.exe` + real
third-party API keys + real Windows ConPTY, none of which work in a
containerised runner. Treat as a manual pre-release check before tagging.

**Refs #541** — the runbook is the deliverable; executing the runtime
verification on a Windows host with real credentials is a separate human
pass.

## Setup

1. **Windows-native host** (non-WSL, non-sandbox — just the default
   `run-dev.ps1` build). The dev profile leaves the stable hub alone;
   only the `buildmesh-dev` process is touched. See `CLAUDE.local.md`
   for the side-by-side layout.
2. **`~/.claude/providers.conf`** populated with valid MiniMax and Kimi
   API keys (per spec #541). The Anthropic subscription works too, but
   the interesting case is MiniMax and Kimi since they're the ones
   that exercise `resolve_provider_env`.
3. **Provider / Harness wiring per ADR-0025** — credentials on
   Providers, base URL + Anthropic model tiers on Harnesses under the
   Claude Code pairing.
4. **A test mesh + node** — use the in-app "New Mesh" flow or
   `tests/e2e/agent-output.spec.ts`'s `create_test_mesh` helper if you
   have the e2e harness around.

**Note on Kimi's auth surface.** Spec #541 treated Kimi as a
Claude-compatible backend (alongside MiniMax). Since #541 was filed,
Kimi moved to its own first-class provider (`Provider::Kimi`),
self-authenticating against `~/.kimi/config.toml` via its native
binary. The multi-line prefill and `ANTHROPIC_*` env assertions
therefore do not apply to Kimi — see [Kimi-specific
expectations](#kimi-specific-expectations) below.

## What this guards

- The direct `claude.exe` → ConPTY path on Windows-native works
  end-to-end (not just at the argv-composition level).
- `resolve_provider_env` (in `preferences/compatibility.rs`) composes
  the backend env, and `apply_routing_env` (in
  `src-tauri/src/agent/spawn/command.rs`) injects the resulting
  `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_MODEL`
  onto the spawn's `CommandBuilder` for the third-party backends.
- Multi-line `--prefill` argv survives through
  `portable_pty::CommandBuilder` into the owned ConPTY without
  truncation (MiniMax arm; Kimi's adapter drops the prefill by design
  — see below).
- `ANTHROPIC.resets_backend_env()` overrides the trait default to
  `true`, and `build_spawn_command_prepared` (in
  `src-tauri/src/agent/spawn/command.rs`) `env_remove`s each entry in
  `CLAUDE_BACKEND_ENV_VARS` before applying the per-profile backend
  env, so a value inherited from buildmesh's own environment cannot
  leak into the spawned `claude.exe` (MiniMax arm; Kimi does not read
  `ANTHROPIC_*`).

## Test cases

Per spec #541: for **each** of MiniMax and Kimi, spawn a node with a
multi-line handover prefill and confirm the prefill lands intact and
the agent authenticates against the right backend.

### Test 1 — Multi-line prefill lands intact

1. Open the test mesh + node you set up above.
2. Trigger a **handover spawn** with the following multi-line prefill
   text (copied verbatim from spec #541):
   ```
   Title: test multi-line prefill

   Line one of the prefill
   Line two — with a dash
   Line three
   ```
3. Within ~5s of spawn, confirm in the TUI that the **entire**
   multi-line prefill appears as the first user turn — not just
   `Title: test multi-line prefill`. The blank line between `Title:`
   and `Line one` must be visible.

**Per-provider expectations:**
- **MiniMax** — passes when the prefill lands intact and the agent
  responds coherently.
- **Kimi** — `KIMI.supports_prefill() == false`, so the orchestrator's
  prefill gate in `spawn_with_intent` (in
  `src-tauri/src/agent/spawn/orchestrator.rs`) emits the WARN
  `spawn_with_intent: provider 'kimi' does not support prefill;
  skipping N bytes` and drops the prefill. Kimi's interactive TUI
  receives its own session bootstrap directly — this is by design, not
  a regression.

### Test 2 — Backend auth + identity query

1. In the spawned terminal, send the agent a model-identity probe:
   ```
   What model are you running? Reply with only the model id, nothing else.
   ```

**Per-provider expectations:**
- **MiniMax** — the response matches the configured backend (e.g.
  `MiniMax-M3[1m]`), NOT an Anthropic-default model like
  `claude-opus-4-6`. An Anthropic-default response indicates
  `ANTHROPIC_BASE_URL` did not reach the spawned `claude.exe`.
- **Kimi** — the response names a Kimi model (e.g. `kimi-k2`,
  `kimi-k2-turbo`). An unknown-model response indicates Kimi's
  `~/.kimi/config.toml` is not picking up the auth — fix the install,
  not buildmesh.

### Test 3 — Inherited backend env is cleared (MiniMax only)

A dev who exported `ANTHROPIC_BASE_URL` in the shell that launches
buildmesh would, without the reset, leak that value into the spawned
`claude.exe` and route through the wrong backend. The Anthropic
adapter's `resets_backend_env()` override (in
`src-tauri/src/agent/provider/adapters/anthropic.rs`) flips the trait
default to `true`; `build_spawn_command_prepared` then `env_remove`s
each entry in `CLAUDE_BACKEND_ENV_VARS` (in
`src-tauri/src/agent/provider/mod.rs`) before applying the
per-profile backend env.

1. Quit buildmesh-dev.
2. In the same shell you'll use to launch buildmesh-dev, set
   `ANTHROPIC_BASE_URL`:
   ```powershell
   $env:ANTHROPIC_BASE_URL = 'https://leaked.example/anthropic'
   ```
3. Launch buildmesh-dev from that shell:
   ```powershell
   scripts\run-dev.ps1
   ```
4. Spawn a MiniMax node (handover flow as in Test 1).
5. Probe the model identity (Test 2's query). The response should be
   the configured MiniMax model, NOT something pointing at
   `leaked.example`. If the leaked value reaches the child, the reset
   path is broken.
6. **Tear down:** unset the env var before continuing:
   ```powershell
   Remove-Item Env:ANTHROPIC_BASE_URL
   ```

### Kimi-specific expectations

`KIMI.resets_backend_env()` inherits the trait default `false` (the
override is `Anthropic`-only). Kimi is not in `CLAUDE_BACKEND_ENV_VARS`
(that list serves the Claude Code adapter's cwrap `unset` parity) and
Kimi does not read `ANTHROPIC_*` env vars at all. Applying Test 3's
leak-guard assertion to a Kimi spawn would falsely report a
regression.

The Kimi half of this runbook therefore asserts only Tests 1 and 2,
with Kimi's Test 1 success signal being the WARN log emitted by
`spawn_with_intent` and Kimi's Test 2 verifying the Kimi model's own
identity (not an Anthropic surface).
