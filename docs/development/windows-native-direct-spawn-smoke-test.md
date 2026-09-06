# Smoke test: Windows-native direct-spawn (Claude-Code-via-MiniMax-backend, Kimi)

Manual runtime verification for the post-#531 cwrap absorption. PR #531 routes
the default Windows path through `claude.exe` spawned directly via the Windows
Pseudo Console (ConPTY) for the first time (previously PowerShell→cwrap). The
argv-prefill path is unit-tested at the composition level (`tests/unit` in
`commands/agent_tests.rs` — `anthropic_prefill_goes_argv_not_env`,
`anthropic_clears_inherited_backend_env`, and
`custom_profile_injects_backend_env_on_windows_native`) but hasn't had
production runtime verification on this path.

**Issue:** #541. **Out of scope for CI:** this requires real `claude.exe` + real
third-party API keys + real Windows ConPTY, none of which work in a containerised
runner. Treat as a manual pre-release check before tagging.

**Refs #541** — this runbook is the deliverable; the actual runtime verification
on a developer's Windows host is a separate manual pass. Issue remains open
until the runbook has been executed and the evidence (per [Evidence
checklist](#evidence-checklist)) attached.

## Setup

1. **Windows-native host** (non-WSL, non-sandbox — just the default
   `run-dev.ps1` build). The dev profile leaves the stable hub alone; only the
   `buildmesh-dev` process is touched. See `CLAUDE.local.md` for the side-by-side
   layout.
2. **`~/.kimi/config.toml` populated** with valid Kimi credentials (Kimi Code
   self-auths via its own config — see [Provider-account wiring — Kimi](#provider-account-wiring--kimi)).
3. **A MiniMax account added via Buildmesh's Accounts UI** AND attached under
   the Claude Code harness with the right endpoint URL + model tiers. See
   [Provider-account wiring — MiniMax](#provider-account-wiring--minimax-claude-code-via-minimax-backend).
4. **A test mesh + node setup** — use the in-app "New Mesh" flow or
   `tests/e2e/agent-output.spec.ts`'s `create_test_mesh` helper if you have
   the e2e harness around.
5. **A `buildmesh.log` watcher ready** — the dev profile writes to
   `%APPDATA%\com.alond.buildmesh.dev\logs\buildmesh.log`. Tail it in a second
   terminal:
   ```powershell
   Get-Content "$env:APPDATA\com.alond.buildmesh.dev\logs\buildmesh.log" -Wait
   ```

## What this guards

- The direct `claude.exe` → ConPTY path on Windows-native works end-to-end
  (not just at the argv-composition level).
- `provider_env()` correctly injects `ANTHROPIC_BASE_URL` /
  `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_MODEL` for the third-party backends
  (Claude-Code-via-MiniMax-backend case).
- Multi-line `--prefill` argv survives through `portable_pty::CommandBuilder`
  into the owned ConPTY without truncation (regression guard for the pre-#531
  cmd.exe truncation bug — Claude-Code-via-MiniMax-backend only).
- `resets_backend_env()` correctly clears inherited `ANTHROPIC_*` (e.g. a dev
  who ran `cwrap --minimax` in the same terminal before launching the app — the
  backend should NOT leak). Claude-Code-via-MiniMax-backend only.

## Provider-account wiring — MiniMax (Claude-Code-via-MiniMax-backend)

The bare legacy provider id `"minimax"` is no longer a first-class executor —
see `Provider::from_db_str` (`src-tauri/src/models/mod.rs`) which routes unknown
ids through to `Provider::Anthropic`. The Anthropic executor then runs
`claude.exe` with backend env injected from the user's **stored pairing** at
spawn time. Per ADR-0025 (`docs/adr/0025-provider-credential-vs-pairing-endpoint.md`):

- Credentials (API key) live on the **Providers** page.
- Endpoint URL and model tiers live on the **Harnesses** page, attached under a
  specific harness.
- "Base URL and model names are set when you attach this provider under
  Harnesses." — `src/components/AppSettings/AppSettingsModal.tsx` (the API-key
  card's helper text).

The UI is a two-tab flow in Settings (`src/components/AppSettings/AppSettingsModal.tsx`,
the `SETTINGS_TABS` list at the top of the file):

### Step A — Add the MiniMax account under Providers

1. Open **Settings → Providers**.
2. If MiniMax is in the first-class catalog (`src/components/AppSettings/AppSettingsModal.tsx`:
   `AddProviderForm`'s catalog list), click it.
3. Otherwise click **Add generic**, and:
   - **Display name:** `MiniMax`.
   - **API key:** your `MINIMAX_API_KEY`.
4. Save. The MiniMax card appears with its API key field and the helper text
   "Base URL and model names are set when you attach this provider under
   Harnesses." Do not look for an Endpoint field — there isn't one on this
   tab.

### Step B — Attach the MiniMax account under Claude Code

1. Open **Settings → Harnesses** (`HarnessConfigList` in
   `src/components/AppSettings/HarnessConfigList.tsx`).
2. Find the **Claude Code** harness card and click **Attach**.
3. Select `MiniMax` from the dropdown.
4. Fill in:
   - **Base URL:** `https://api.minimax.io/anthropic` (or the alternative URL
     from `~/.claude/providers.conf`'s `MINIMAX_BASE_URL`).
   - **Default model** (Anthropic-surface tier map): `MiniMax-M3[1m]`. The
     other Anthropic tiers (Fable / Opus / Sonnet / Haiku / Small-fast) are
     derived from `preferences::minimax_default_tiers` (re-exported via
     `src-tauri/src/preferences/mod.rs` and defined in
     `src-tauri/src/preferences/resolver/catalog.rs`).
5. Save. Confirm "MiniMax" appears under **Claude Code** in the Spawn Menu.

A user who has not completed both steps will not see "MiniMax" in the Spawn
Menu — that's the symptom of a missing prerequisite, not a #531 regression.

## Provider-account wiring — Kimi

Kimi Code is a **native** self-auth harness (`Provider::Kimi` → `KIMI` adapter
in `src-tauri/src/agent/provider/adapters/kimi.rs`). Per `Provider::from_db_str`
and the harness roster at `src-tauri/src/models/mod.rs` (`Provider::Kimi`
variant), Kimi is its own enum arm — it does **not** route through the
`anthropic` adapter and does **not** use `provider_env()` / `ANTHROPIC_*` env
injection. Its `~/.kimi/config.toml` owns auth (issue #918 / wayfinder #908).

For this smoke test, Kimi's auth path is:

1. Install `kimi` from Moonshot's installer (creates `~/.kimi/config.toml`).
2. Run `kimi` once interactively and complete the login flow.
3. Confirm `kimi --version` resolves on PATH:
   ```powershell
   kimi --version
   ```
   If this fails, the smoke test cannot spawn Kimi — fix the install first.

There is **no Buildmesh-side credential UI for Kimi** — Kimi is not a
proxied provider, so the `Harnesses` page does not show an Attach control for
it. Spawning it from the Buildmesh Spawn Menu launches the local `kimi` binary
directly via the `KIMI` adapter's `spawn_recipe` (`binary = "kimi"`,
`windows_shell = WindowsShell::Direct`).

## Test cases

The tests below are **per provider** because the two halves of #541 exercise
different adapters and different runtime paths. Kimi's tests intentionally do
not cover prefill or backend-env-reset — those concepts don't apply to a
native binary that doesn't read `ANTHROPIC_*`.

### Tests for Claude-Code-via-MiniMax-backend

#### Test 1 — Multi-line prefill lands intact

1. Open the test mesh + node you set up above.
2. Trigger a **handover spawn** with the following multi-line prefill text
   (copied verbatim — the issue #541 fixture):
   ```
   Title: test multi-line prefill

   Line one of the prefill
   Line two — with a dash
   Line three
   ```
   The exact UI gesture depends on your build (right-click a terminal → "Spawn
   handover agent" or the omnibar's handover flow); the goal is to drive
   `SpawnAgentIntent::Handover` through `spawn_with_intent` in
   `src-tauri/src/agent/spawn/orchestrator.rs`.
3. Within ~5s of spawn, confirm in the TUI:

   **(a) Full prefill lands.** The *entire* multi-line prefill appears in the
   TUI as the first user turn, not just `Title: test multi-line prefill`
   (which is what the old `BUILDMESH_PREFILL` env-transport path would have
   produced if cmd.exe had truncated at the first newline). The blank line
   between `Title:` and `Line one` must be visible.

   **(b) Backend auth.** The agent authenticates against the MiniMax backend —
   you should see the right model (e.g. `MiniMax-M3[1m]`) in the activity bar
   or transcript. The agent should respond coherently to a non-Anthropic-tuned
   prompt (MiniMax serves Claude Code with a swapped base URL, not Anthropic's
   first-party API).

#### Test 2 — Backend env injection

1. Confirm in `buildmesh.log` that the spawn carries the right `ANTHROPIC_*`
   env vars. After spawning, the orchestrator emits a structured spawn log
   line that includes the resolved `CommandBuilder`'s env. Grep for the most
   recent spawn for your node id:
   ```powershell
   Select-String -Path "$env:APPDATA\com.alond.buildmesh.dev\logs\buildmesh.log" -Pattern "spawn.*env|session.*spawn" | Select-Object -Last 20
   ```
2. You should see (at minimum):
   - `ANTHROPIC_BASE_URL=https://api.minimax.io/anthropic`
   - `ANTHROPIC_AUTH_TOKEN=<your key>` (truncated in the log; presence is the
     signal, not the value)
   - `ANTHROPIC_MODEL=MiniMax-M3[1m]`

#### Test 3 — Backend identity query

1. In the spawned terminal, send the agent a query that proves which model it
   is running. The recommended probe:
   ```
   What model are you running? Reply with only the model id, nothing else.
   ```
2. The response should match the configured backend (e.g. `MiniMax-M3[1m]`),
   NOT an Anthropic-default model like `claude-opus-4-6`. An
   Anthropic-default response indicates `ANTHROPIC_BASE_URL` did not reach
   the spawned `claude.exe` — see Test 4 for the env-leak guard.

#### Test 4 — Inherited backend env is cleared

This is the regression guard for the `resets_backend_env()` path. A dev who
ran `cwrap --minimax` in the same terminal before launching buildmesh would
have an inherited `ANTHROPIC_BASE_URL` in buildmesh's own environment —
pre-#531 that would have leaked into the spawned `claude.exe` and routed
through the wrong backend.

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
5. Probe the model identity (Test 3's query). The response should be the
   configured MiniMax model, NOT something pointing at `leaked.example`. The
   `ANTHROPIC_BASE_URL` clear (the `CLAUDE_BACKEND_ENV_VARS` reset at
   `env_remove` in `build_spawn_command_prepared` — see
   `src-tauri/src/agent/spawn/command.rs`, where `claude_direct_recipe`'s
   `resets_backend_env` flag drives the reset) must win over the inherited
   value.
6. **Confirm in the log** that the spawn's `CommandBuilder` env does NOT
   carry the inherited value. The structured spawn log line for this spawn
   should show `ANTHROPIC_BASE_URL=<the configured endpoint>` and never
   `https://leaked.example/...`. If the leaked value appears, the
   `resets_backend_env()` path is broken and PR #531's whole env-leak
   guarantee has regressed.
7. **Tear down:** unset the env var (and the test value you set) before
   continuing:
   ```powershell
   Remove-Item Env:ANTHROPIC_BASE_URL
   ```

### Tests for Kimi (native binary)

These tests cover the parts of the Kimi spawn path that #531 touched — namely
that `KIMI.spawn_recipe()` runs the binary directly under ConPTY instead of
through any wrapper. Prefill, `provider_env`, and `resets_backend_env` do not
apply to Kimi (see [Expected behaviour for Kimi prefill](#expected-behaviour-for-kimi-prefill)
and [Expected behaviour for Kimi env reset](#expected-behaviour-for-kimi-env-reset)).

#### Test 5 — Kimi binary launches directly

1. From the Spawn Menu, select the **Kimi Code** harness (no provider pairing
   needed).
2. Spawn an agent node.
3. Confirm:
   - The PTY renders the Kimi TUI within ~5s.
   - The agent is interactive (Kimi's TUI is launched in interactive mode —
     not `-p` non-interactive).
4. Confirm in `buildmesh.log` that the spawn line records
   `binary = "kimi"`, `windows_shell = Direct` (matching the
   `KIMI.spawn_recipe()` shape).

#### Test 6 — Kimi auth (no Buildmesh credential involvement)

1. In the Kimi TUI, send a query that proves which model it is running. The
   recommended probe:
   ```
   What model are you running? Reply with only the model id, nothing else.
   ```
2. The response should name a Kimi model (e.g. `kimi-k2`, `kimi-k2-turbo`).
   An unknown-model response indicates Kimi's `~/.kimi/config.toml` is not
   picking up the auth — fix the install, not buildmesh.

## Expected behaviour for Kimi prefill

Kimi's adapter declares `KIMI.supports_prefill() == false` (see
`src-tauri/src/agent/provider/adapters/kimi.rs`). The orchestrator's prefill
gate in `spawn_with_intent` (`src-tauri/src/agent/spawn/orchestrator.rs`)
emits:

```
WARN spawn_with_intent: provider 'kimi' does not support prefill; skipping N bytes
```

and drops the prefill entirely. **This is by design, not a #541 regression.**
Kimi's interactive TUI receives its own session bootstrap directly — it does
not accept a `--prefill` flag and there is no equivalent transport today.

For the Kimi half of this smoke test, the prefill fixture from Test 1 is
**N/A**: do not assert that Kimi received the multi-line prefill. The Test 5
spawn step above uses no prefill at all.

If a future change adds a Kimi prefill transport, this runbook will need a
revisit (and Kimi's adapter should flip `supports_prefill()` to `true`).

## Expected behaviour for Kimi env reset

Kimi's adapter inherits the trait default `resets_backend_env() = false`
(defined in `src-tauri/src/agent/provider/mod.rs` as the default impl). Kimi
is not in `CLAUDE_BACKEND_ENV_VARS` (that list is for the Claude Code
adapter's cwrap `unset` parity) and Kimi does not read `ANTHROPIC_*` env
vars at all. The Test 4 leak-guard assertion **must not be applied to Kimi
spawns** — it would falsely report a regression.

If a future change adds Kimi-specific env-var handling, this runbook will
need a revisit and a Kimi-specific test added.

## Failure-mode catalogue

### MiniMax (Claude-Code-via-MiniMax-backend)

| Symptom | Likely cause |
|---|---|
| Spawn logs `claude.exe not found` | `claude.exe` is not on PATH for the user buildmesh-dev runs as. Install Claude Code or fix PATH. |
| MiniMax spawn authenticates as Anthropic default | `ANTHROPIC_BASE_URL` was not injected — the stored pairing is missing or has an empty base URL, or `provider_env()` returned empty. Re-check Step B of the MiniMax wiring above. |
| Prefill truncated at first newline | The argv survived but ConPTY is splitting on `\n` — unlikely on a fresh direct-spawn build. Reproduce in a fresh `run-dev.ps1` run; if persistent, file with `buildmesh.log` slice. |
| `WARN ... does not support prefill` for MiniMax (Anthropic-backed) | `Provider::Anthropic` adapter is missing or its `supports_prefill()` returned `false` — both should be `true`. File with log slice; this is a regression from the unit test `prefill_appended_for_supporting_provider`. |
| `leaked.example` URL survives into the child | `resets_backend_env()` is no longer firing for `Provider::Anthropic`. Check `KIMI`-equivalent code path — the `Anthropic` adapter's `resets_backend_env` (in `src-tauri/src/agent/provider/adapters/anthropic.rs`) is still `true`. |

### Kimi (native binary)

| Symptom | Likely cause |
|---|---|
| Kimi spawn fails immediately with "kimi: command not found" | Kimi not installed or not on PATH. Fix the install; nothing in this runbook exercises the Kimi spawn path beyond a successful binary launch. |
| Kimi spawn succeeds but reports an unknown model | Kimi's `~/.kimi/config.toml` is not picking up auth. Fix the install; this is not a buildmesh regression. |
| Kimi prefill test fires and reports a regression | Re-read [Expected behaviour for Kimi prefill](#expected-behaviour-for-kimi-prefill) — this is **not** a regression. |
| Kimi env-reset test fires and reports a regression | Re-read [Expected behaviour for Kimi env reset](#expected-behaviour-for-kimi-env-reset) — this is **not** a regression. The `ANTHROPIC_*` env-leak guard does not apply to Kimi. |

## Evidence checklist (attach to the PR closing #541)

When you complete this smoke test, attach to the PR:

### MiniMax (Claude-Code-via-MiniMax-backend)

- [ ] One full screenshot of the agent's first user turn showing the multi-line
      prefill intact (Test 1a).
- [ ] A `buildmesh.log` slice covering the spawn. Include both the structured
      spawn-line and the post-spawn reader-thread start line (Test 1 + 2).
- [ ] The model's reply to the "What model are you running?" probe (Test 3).
- [ ] For Test 4: the spawned env dump showing `ANTHROPIC_BASE_URL` resolved to
      the configured endpoint (not the leaked shell value), AND the model
      reply confirming backend identity.

### Kimi (native binary)

- [ ] A `buildmesh.log` slice covering a Kimi spawn. Confirm `binary = "kimi"`,
      `windows_shell = Direct` (Test 5).
- [ ] The Kimi model's reply to the "What model are you running?" probe
      (Test 6).

A closing comment on issue #541 should link the PR and call out any failures
from the catalogue above.
