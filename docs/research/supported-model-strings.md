# Supported model strings for Buildmesh harnesses

**Status:** Current research snapshot, checked 2026-09-26. Model catalogs and
account access change over time; use each installed harness's own catalog or
model picker as the final source for a saved configuration.

## How Buildmesh passes model strings

Buildmesh only passes a Launch Configuration's **Model** value to harnesses
whose adapter advertises model overrides. It sends the value as one CLI
argument, without translating it to a Buildmesh-wide model name. The accepted
syntax therefore belongs to the selected harness, provider, and account. For a
proxied provider route, use the exact model ID accepted by that endpoint.
Buildmesh can list known provider models and verifies Codex routes, but it does
not maintain one complete model list for every native CLI. See the [Launch
Configuration workflow](../user-guide.md#launch-configurations), the
[Launch Configuration resolver](../../src-tauri/src/agent/launch.rs), and the
[harness capability matrix](../learning/harness-capabilities-matrix.md).

The examples below are useful values to start with, not promises that every
account can use every model. Entitlements, region, team restrictions, provider
configuration, and CLI version can change the available set.

## Harnesses with a Buildmesh model override

| Harness | Value passed by Buildmesh | Supported value form and examples | Find the exact value for your install |
|---|---|---|---|
| **Claude Code** | `--model <value>` | Aliases include `sonnet`, `opus`, `haiku`, and `fable`; `sonnet[1m]`, `opus[1m]`, `best`, and `opusplan` are also documented. Anthropic API IDs currently include `claude-sonnet-5`, `claude-opus-5-5`, `claude-fable-5-1`, and `claude-haiku-4-5`. Alias targets vary by provider; use a provider-specific deployment/model ID with a gateway or compatible endpoint. | Run Claude Code and use `/model` to see available models. Full model IDs and alias behavior are in [Claude Code model configuration](https://code.claude.com/docs/en/model-config) and the [Anthropic model catalog](https://platform.claude.com/docs/en/models/overview). Buildmesh passes the chosen value through the [Anthropic adapter](../../src-tauri/src/agent/provider/adapters/anthropic.rs). |
| **Codex** | `--model <value>` | A free-form model string from the active Codex catalog/provider. OpenAI's current Codex config example is `gpt-6-sol`; the current family also includes `gpt-6-astra` and `gpt-6-luna`. Custom Codex providers can define their own IDs. | Use the model selector in Codex and check the configured provider. The [Codex config guide](https://learn.chatgpt.com/docs/config-file/config-basic) and [config reference](https://learn.chatgpt.com/docs/config-file/config-reference) document the model string and custom model catalog. The account's available list may be narrower than OpenAI's general [model catalog](https://developers.openai.com/api/docs/models/all). For a proxied route, Buildmesh verifies the exact endpoint/model/runtime combination before launch. See the [Codex adapter](../../src-tauri/src/agent/provider/adapters/codex.rs). |
| **Antigravity** | `--model <value>` | Use a model slug from the Antigravity catalog, for example `gemini-3.8-flash-high`, `gemini-3.8-flash-medium`, `gemini-3.7-flash-high`, `gemini-3.6-flash-high`, `gemini-3.1-pro-high`, or `claude-sonnet-4-6`. Unknown slugs can fail when the CLI starts a request. | Run `agy models` in the same Windows/WSL runtime as the Buildmesh harness. The [Antigravity model catalog](https://antigravity.google/docs/models) and [headless CLI guide](https://antigravity.google/docs/cli/headless/) document model selection. See the [Antigravity adapter](../../src-tauri/src/agent/provider/adapters/agy.rs). |
| **OpenCode** | `--model <value>` | Use the case-sensitive form `provider_id/model_id`; a provider-supported variant can follow `#`, for example `openai/gpt-5.2#high`. The catalog is dynamic across enabled, authenticated providers, and custom models are supported. | Run `opencode models` (optionally with a provider ID). Copy the exact value it prints. See [OpenCode model selection](https://opencode.ai/v2/docs/models), [CLI commands](https://opencode.ai/v2/docs/cli/commands), and the [OpenCode adapter](../../src-tauri/src/agent/provider/adapters/opencode.rs). |
| **Grok Code** | `--model <value>` | Use a Grok Build catalog ID or a configured model alias. Current examples are `grok-build` and `grok-4.7`; custom aliases can map to a model ID and endpoint in `~/.grok/config.toml`. Avoid `grok-code-fast-1` for new configurations: xAI retired it on 2026-05-15 and redirects it to `grok-build-0.1`. | Run `grok models` for the live list. See xAI's [CLI reference](https://docs.x.ai/build/cli/reference), [Grok Build settings](https://docs.x.ai/build/settings), [current model catalog](https://docs.x.ai/developers/models), and [retirement notice](https://docs.x.ai/developers/migration/may-15-retirement). Buildmesh's [Grok adapter](../../src-tauri/src/agent/provider/adapters/grok.rs) forwards the value. |
| **Cursor** | `--model <value>` | Cursor does not document a complete fixed allowlist for the CLI. Its CLI examples include `gpt-5`; the actual list depends on account, plan, region, and team model controls. | Use `/model` in Cursor Agent to list available choices. The [Cursor CLI overview](https://prod.cursor.com/docs/cli/overview) and [slash-command reference](https://prod.cursor.com/docs/cli/reference/slash-commands) document selection; the [availability page](https://prod.cursor.com/help/models-and-usage/available-models) covers account access. See the [Cursor adapter](../../src-tauri/src/agent/provider/adapters/cursor.rs). |
| **Kimi Code** | `-m <value>` | Use a **configured model alias** from Kimi Code's `models` configuration; it need not be the upstream API model ID. Managed aliases include `kimi-code/k3`, `kimi-code/kimi-for-coding`, and `kimi-code/kimi-for-coding-highspeed`. | Run `kimi provider list --json` or inspect `~/.kimi-code/config.toml` (or `config.toml` under `KIMI_CODE_HOME` when set) and copy an alias from its `[models]` table. See Kimi's [CLI reference](https://moonshotai.github.io/kimi-code/en/reference/kimi-command), [configuration guide](https://moonshotai.github.io/kimi-code/en/configuration/config-files), and [provider guide](https://moonshotai.github.io/kimi-code/en/configuration/providers.html). See the [Kimi adapter](../../src-tauri/src/agent/provider/adapters/kimi.rs). |
| **Command Code** | `--model <value>` | Use an ID in Command Code's model registry. Current examples include `deepseek/deepseek-v4-flash`, `claude-sonnet-5`, `gpt-6-sol`, `moonshotai/Kimi-K2.7-Code`, and `z-ai/glm-5.3-flash`. Unknown IDs are rejected. The registry accepts a full ID or its suffix after `/`, case-insensitively. | Run `cmd --list-models` (on Windows Buildmesh launches `cmdc`, but the documented Command Code command is `cmd`). The output matches the `/model` picker. See the [Command Code model reference](https://commandcode.ai/docs/reference/cli/models) and the [Buildmesh adapter](../../src-tauri/src/agent/provider/adapters/commandcode.rs). |
| **Meta Muse** | `--model <value>` | Official IDs include `muse-spark-1.3`, `muse-spark-1.3-contributor`, `muse-spark-1.2`, `muse-spark-1.2-contributor`, and `muse-spark-1.1`. The CLI accepts an arbitrary string locally, but the model must still be available to the configured Meta account/API. | Use `/models` in Muse Code. The [Muse Code configuration guide](https://dev.meta.ai/docs/muse-code/configuration), [Meta model overview](https://dev.meta.ai/docs/overview), and [Muse changelog](https://dev.meta.ai/docs/muse-code/changelog) list current model behavior. See the [Muse adapter](../../src-tauri/src/agent/provider/adapters/muse.rs). |
| **Cline** | `--model <value>` | The model ID is interpreted by Cline's selected provider. Direct-provider IDs can be bare model IDs such as `claude-sonnet-5`; a gateway such as Cline's provider may use qualified IDs such as `anthropic/claude-sonnet-4-6`. Buildmesh sends the model but does not add Cline's `--provider` option. | Configure the provider in Cline first, then copy a model ID from that provider's catalog/configuration. Use `cline --help` for the active CLI flags and `cline config` to inspect its setup. See the [Cline CLI reference](https://github.com/cline/cline/blob/main/docs/cli/cli-reference.mdx), [model ID guide](https://github.com/cline/cline/blob/main/docs/api/models.mdx), and [Buildmesh Cline adapter](../../src-tauri/src/agent/provider/adapters/cline.rs). |

## Harnesses without a Buildmesh model override

These harnesses do not accept a model value from Buildmesh Launch Configurations. Where the upstream CLI has its own model selector or provider settings, configure the model inside that harness instead.

| Harness | Why Launch Configurations cannot set the model | Upstream model selection |
|---|---|---|
| **MiniMax Code** | Buildmesh starts MiniMax Code's interactive TUI. Its `--model` flag belongs to the separate `mcode exec` mode, which Buildmesh does not launch. | Use `/provider` in the TUI or run `mcode provider list --json` to inspect configured providers and model IDs. Headless IDs use `provider-id/model-id`. See the [MiniMax Code CLI](https://github.com/MiniMax-AI/minimax-code/blob/main/README.md), [examples](https://github.com/MiniMax-AI/minimax-code/blob/main/docs/examples.md), and [demo](https://github.com/MiniMax-AI/minimax-code/blob/main/docs/demo.md), plus the [Buildmesh adapter](../../src-tauri/src/agent/provider/adapters/mcode.rs). |
| **DeepSeek Harness** | Buildmesh launches `dsh` without advertising a model override; provider and model selection belong to the harness profile. | Select a model in DeepSeek Harness **Settings → Models**. Its built-in DeepSeek provider currently documents `deepseek-flash` and `deepseek-v4-pro`; custom providers have their own model IDs. No public CLI model-list command is documented. See the [provider guide](https://deepseek-harness.github.io/deepseek-harness/en/guide/providers), [DeepSeek provider package](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/llm/llm-deepseek/README.md), and [API updates](https://api-docs.deepseek.com/updates/). This is separate from Buildmesh's DeepSeek provider routes for Claude Code and Codex. See the [Buildmesh adapter](../../src-tauri/src/agent/provider/adapters/dsh.rs). |
| **Freebuff** | Buildmesh does not expose a model override for this harness. | Choose a model in Freebuff's picker. Current source-catalog examples include `z-ai/glm-5.3-flash`, `deepseek/deepseek-v4.1-flash`, `openai/gpt-6-luna`, `mimo/mimo-v2.6-pro`, and `meta/muse-spark-1.3-contributor`. A bundled Freebuff release may contain an older catalog; use its picker. See the [Freebuff repository](https://github.com/CodebuffAI/freebuff), [model catalog](https://github.com/CodebuffAI/freebuff/blob/main/common/src/constants/freebuff-models.ts), and [Buildmesh adapter](../../src-tauri/src/agent/provider/adapters/freebuff.rs). |
| **Terminal** | Terminal is a shell, not an LLM harness, so there is no model argument or model catalog. | None. See the [Buildmesh Terminal adapter](../../src-tauri/src/agent/provider/adapters/terminal.rs). |

## Creating configurations

For supported overrides, open **Settings → Launch Configurations**, select the
harness and authentication/provider route, then use the exact ID or alias from
that row's live selector/list. If using a custom or proxied endpoint, use the
model identifier that endpoint expects. A model that the CLI accepts may still
be unavailable to the selected account or route.

For MiniMax Code, DeepSeek Harness, or Freebuff, save the model in that
harness's own provider/profile settings or use its model picker; Buildmesh
cannot store a per-launch model for those interactive launch modes.
