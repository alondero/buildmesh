# Harness attention reliability audit

Investigated 2026-09-11/12. This is a contract/source audit of the support present at the start of the work and the fixes landed in this change. It is not a measured delivery-rate benchmark. No paid model calls were made. Hook registration, configuration acceptance, actual callback delivery, and correct UI transitions are separate evidence levels.

## What a consistent experience requires

Turn completion means the foreground turn ended; it does not necessarily mean the entire task succeeded or that a human decision is pending. A structured question and a permission approval are separate attention reasons. Background work, errors, interruptions, and ordinary informational notifications must not be conflated with any of those.

The local source of truth is the [provider inventory](../../src-tauri/src/agent/provider/adapters/mod.rs), [attention route](../../src-tauri/src/http/routes/attention.rs), [capability defaults](../../src-tauri/src/agent/provider/mod.rs), and [attention autoclear](../../src-tauri/src/attention_autoclear.rs). A `requires_attention_hook = false` adapter does not acquire reliable turn detection just because it produces PTY output. Quiet output is ambiguous; redraw output does not establish that a pending question was answered.

## Inventory at investigation start

| Harness | Existing Buildmesh mechanism | Reliability assessment / verified gap |
| --- | --- | --- |
| Anthropic / Claude Code | Project hooks and readable transcripts | Structured completion available. Prompt questions, MCP elicitation, informational notifications, and stop-gate continuation need explicit handling. |
| Claude-compatible model profiles, including MiniMax through Claude | Execute through Anthropic adapter | Same hook contract as Claude; the model account does not define the lifecycle contract. |
| Codex, including compatible model profiles | Native hooks, legacy notify support, trust provisioning | Structured completion/approval path exists. Explicit user-input tools need separate handling; hook config/version/runtime home must match the launched CLI. |
| Antigravity (`agy`) | Project `.agents/hooks.json`, `Stop` | Completion has useful `fullyIdle` distinction. Mid-turn `ask_question` / `ask_permission` are absent from provisioned hooks. No documented generic permission notification hook. |
| Cursor | Project `.cursor/hooks.json`, `stop`; launch uses `--force` | Completion supported by contract. Do not infer an actual approval prompt from generic pre-tool events. Question support requires a verified CLI tool/event contract. |
| OpenCode | Local ESM event plugin | Existing plugin forwards creation, idle, and permission requests; misses questions and resumption events. Idle was described/classified as input-required, conflating completion with questions. |
| Grok | Global native HTTP hooks | Direct completion/notification path exists. Error/cancel outcomes and late callbacks need explicit handling. Question tool is `ask_user_question`. |
| Command Code | Passive transcript watcher, no native attention hooks | Completion is not entirely unsupported: watcher is already present. Official native `Stop`/`SessionStart` hooks provide another verified integration surface. |
| Kimi | No attention hook at audit start | Installed native Kimi Code has hooks, including real permission events; this is a concrete missing integration. Native Kimi Code and legacy Python kimi-cli are different products/config roots. |
| MiniMax Code (`mcode`) | No attention hooks | Installed 0.2.7 contains two hook systems with different event catalogs. Native hook capability exists, but a validated TUI delivery/config route is still required. |
| Meta Muse | No attention hooks; readable event storage used for session/usage support | Installed 1.1.1 has structured logs and an MSP server, but no verified interactive attention-hook registration. A log watcher or MSP adapter needs real lifecycle fixtures. |
| Freebuff | No hooks or passive turn watcher | No first-party external lifecycle-hook contract established by this audit. React hooks and SDK callbacks are not equivalent to TUI lifecycle notifications. |
| DeepSeek Harness (`dsh`) | Capabilities deliberately gated | Launcher selects plugin profiles, not one fixed TUI contract. No profile is validated by this adapter; hooks cannot honestly be promised generically. |
| Terminal | Plain shell | No AI turn/question semantics. Process termination is observable; agent completion inside a persistent shell is not. |
| Dynamic harness entries | Capabilities resolve through a concrete adapter | Inherit that adapter only when the executable actually implements its contract. A custom binary/name does not establish native attention support. |

The table's Buildmesh claims are grounded in each corresponding [adapter](../../src-tauri/src/agent/provider/adapters), plus the [Command Code watcher](../../src-tauri/src/services/commandcode_watcher.rs). It records the starting inventory; the implemented status and evidence follow below.

## Implemented status

| Harness | Final lifecycle coverage | Reliability boundary |
| --- | --- | --- |
| Claude / Anthropic profiles | Stop, permission, MCP elicitation, prompt submission, question-tool pre/post/failure, and failure callbacks | High-confidence structured signals; transcript scan still degrades when the file is unavailable. |
| Codex | SessionStart capture, Stop, permission, prompt submission, and native `request_user_input` pre/post hooks | High on Codex Code **0.154.0+** with the matching native config root; older or differently launched binaries are not silently upgraded. |
| Antigravity | Stop with `fullyIdle` and transcript pending-task suppression | Completion is reliable when the documented payload is present; question/permission tools remain ungated because their pre-tool response is a decision gate, not a neutral observer. |
| Cursor | Stop completion and transcript pending-task suppression | Completion-only. Cursor CLI's `AskQuestion` bypasses pre/post hooks, so Buildmesh does not claim question detection from generic tool events. |
| OpenCode | Plugin idle/busy, permission, question asked/replied/rejected, session creation, and parent/child correlation | High for the documented plugin event stream; a missing or disabled plugin falls back to the explicit degraded path. |
| Grok | Stop, idle/task notifications, prompt submission, failures/cancellation, and `ask_user_question` pre/post/failure | High when the native HTTP hook is provisioned and the runtime token is presented; callbacks from another process are rejected. |
| Kimi Code | Native 0.27 hooks for Stop/failure/interruption, permissions, prompt submission, question/plan pre/post/failure, and terminal background-task notifications | High on native Kimi Code 0.27+; background questions are correlated by task id, their early PostToolUse is not treated as resumed work, and terminal completion lands in Ready. Python `kimi-cli` is a separate unsupported product. |
| Command Code | Passive transcript watcher and existing lifecycle classifier | Medium completion confidence; no native question observer is claimed without a verified delivery fixture. |
| MiniMax Code, Muse, Freebuff, DeepSeek Harness, Terminal | Explicit capability gaps remain | No guessed native hook is installed. These harnesses need a validated plugin, MSP/log, profile, or protocol integration before Buildmesh can promise parity. |
| Dynamic/proxied entries | Resolve to the concrete adapter selected at spawn | A custom executable/name inherits a contract only when it actually implements that adapter's verified protocol. |

Every accepted callback now passes through the shared attention route, records provider event/session/health metadata, fences stale turn/session ids, and updates both desktop and mobile lifecycle transports. Per-node ordering state retains multiple outstanding questions, preserves Kimi background questions across new prompts, and prevents delayed callbacks from reviving terminal nodes. Structured question/permission marks disarm the output-based autoclear safety net; only generic degraded marks use that heuristic.

## Primary contracts and rationale

### Claude and Codex

Claude exposes `Stop`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, notifications and MCP elicitation hooks. A `PreToolUse` callback should flag a question only for an actual question tool, not arbitrary tools. `Stop` can be blocked by another hook, so receipt is not proof that no continuation will occur. Notify-only observers must preserve permission decisions. [Claude hook reference](https://code.claude.com/docs/en/hooks)

Codex has a native hooks contract in addition to older completion notification integrations. The adapter now provisions the actual `request_user_input` matcher separately from completion and approval. The selected config root and executable version matter, especially for WSL or model-provider profiles. [Codex hooks](https://developers.openai.com/codex/hooks/), [0.154.0 request-user-input handler](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/core/src/tools/handlers/request_user_input.rs), [0.154.0 hook registry](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/core/src/tools/registry.rs)

### Antigravity and Cursor

Antigravity's native events are `PreToolUse`, `PostToolUse`, `PreInvocation`, `PostInvocation`, `Stop`. `Stop` carries `executionNum`, `terminationReason`, optional `error`, and `fullyIdle`; common fields include `conversationId` and `transcriptPath`. Question and scoped-permission tools are `ask_question` and `ask_permission`. Their pre-tool input uses `toolCall.name` / `toolCall.args`; the documented payload does not identify the hook event. A forwarder therefore needs an event discriminator. Crucially, pre-tool output is a permission gate: automatically returning `allow` is not neutral. Verify a non-mutating response before provisioning observation hooks. [Antigravity hooks](https://antigravity.google/docs/hooks)

Cursor documents `stop`, `beforeSubmitPrompt`, generic pre/post-tool hooks and session events, with a versioned `.cursor/hooks.json` format. Documentation covering IDE Agent does not itself prove every event is implemented in the CLI. In particular, Cursor's CLI `AskQuestion` bypasses pre/post hooks, so generic tool observation cannot establish a question request. The existing adapter's stop integration has a narrower claim than full human-input detection. [Cursor hooks](https://prod.cursor.com/docs/hooks), [Cursor AskQuestion hook behavior](https://forum.cursor.com/t/cursor-cli-askquestion-tool-skips-pretooluse-and-posttooluse-hooks/161836/6)

### OpenCode

Plugins receive structured events; documented `session.idle`, `session.status`, `permission.asked` and `permission.replied` permit completion and permission transitions. The actual question module additionally publishes `question.asked`, `question.replied`, and `question.rejected`. A parent session and its children must not overwrite each other's attention. Session creation alone is insufficient for resume binding. Pending questions must survive an idle event until answered/rejected. [Plugin contract](https://opencode.ai/docs/plugins/), [question source](https://raw.githubusercontent.com/anomalyco/opencode/dev/packages/opencode/src/question/index.ts)

### Kimi: version and configuration verified

Native `kimi --version` returned **0.27.0**. That release's docs specify `~/.kimi-code/config.toml`, relocated by `KIMI_CODE_HOME`, with strict `[[hooks]]` fields `event`, `matcher`, `command`, `timeout`. Required events already exist: `Stop`, `StopFailure`, `Interrupt`, `PermissionRequest`, `PermissionResult`, `SessionStart`, `UserPromptSubmit`, pre/post-tool events. `Notification` describes background task changes, not general permissions. Newer `TurnStarted` must not be assumed on 0.27.0. [Pinned hook contract](https://raw.githubusercontent.com/MoonshotAI/kimi-code/@moonshot-ai/kimi-code@0.27.0/docs/en/customization/hooks.md)

`kimi doctor config .tmp/kimi-attention-contract.toml` passed with the production event registrations, without starting a model; the focused ignored test exercised the installed binary. Source uses `shell: true`; inherited environment and hidden Windows windows are explicit in spawn options. Local session directory names include `session_<UUID>`, so generic UUID parsing alone is insufficient. [Pinned runner](https://raw.githubusercontent.com/MoonshotAI/kimi-code/@moonshot-ai/kimi-code@0.27.0/packages/agent-core-v2/src/agent/externalHooks/runner.ts)

The native tools include `AskUserQuestion` and `ExitPlanMode`; background questions may remain open after the turn ends. Do not clear them solely on Stop. Kimi's background tool result supplies a task id, while a later `Notification` with `source_kind=background_task` and terminal `task.*` status resolves it; delivery can be notification-before-result or result-before-notification. [Native tools](https://raw.githubusercontent.com/MoonshotAI/kimi-code/main/docs/en/reference/tools.md), [pinned question implementation](https://raw.githubusercontent.com/MoonshotAI/kimi-code/@moonshot-ai/kimi-code@0.27.0/packages/agent-core-v2/src/agent/questionTools/tools/ask-user.ts), [pinned hook translation](https://raw.githubusercontent.com/MoonshotAI/kimi-code/@moonshot-ai/kimi-code@0.27.0/packages/agent-core-v2/src/agent/externalHooks/externalHooksService.ts)

Legacy Python kimi-cli instead uses `~/.kimi/config.toml` / `KIMI_SHARE_DIR`; its similarly named hook system must not be provisioned accidentally for the native binary. [Legacy config source](https://raw.githubusercontent.com/MoonshotAI/kimi-cli/main/src/kimi_cli/config.py)

### Grok

The installed first-party guide `C:/Users/alond/.grok/docs/user-guide/10-hooks.md` documents five registrations for busy/idle coverage: `UserPromptSubmit`, `Stop`, `StopFailure`, `StopCancelled`, and `Notification` matching `idle_prompt`. The latter is a roughly minute-delayed backstop, including error/interrupt outcomes. `StopFailure` carries `error`, `errorDetails`, `lastAssistantMessage`; cancellation carries `reason`, `reasonDetails`. Both can arrive late. `promptId`, not dispatch timestamp, orders turns; session correlation alone does not fence an older turn from the same session.

The same guide warns that Stop is a gate: another hook can cause a continuation without another user prompt. The installed `03-keyboard-shortcuts.md` identifies the question tool as `ask_user_question`. Native HTTP hooks avoid shell quoting, but still depend on loopback reachability across Windows/WSL. Public entry point: [Grok CLI reference](https://docs.x.ai/build/cli/reference). These detailed claims derive from the installed first-party guides, not a live model test.

### Command Code

Native `Stop` and `SessionStart` use nested command-hook groups in `.commandcode/settings.json` or the global equivalent. Omit `matcher` for these lifecycle events: a matcher prevents them firing. Empty stdout with exit zero is neutral. Existing transcript watching is still a real completion path; introducing native hooks requires deduplication and must not discard successful live callbacks when a transcript fallback is missing. [Command Code hooks](https://commandcode.ai/docs/hooks), [mods lifecycle](https://commandcode.ai/docs/mods)

### Remaining harnesses

Installed MiniMax Code package **0.2.7** has a first-party changelog saying lifecycle plugin hooks arrived in 0.2.4 and tool-name/input compatibility fixes in 0.2.5. Its packaged `chunks/chunk-BPRXRI4W.js` lists `Stop` and `PermissionRequest`; `chunk-CSEMCTUO.js` has a separate daemon hook catalog including `MessageComplete`, and warns unsupported events will never fire. Do not register Stop into the latter and claim success. That second bundle also implements compatible plugin manifests (`.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`) with default `hooks/hooks.json`, a promising integration path whose activation still needs validation. The first-party portable example is explicitly a structural preview, not a proven integration. [MiniMax plugin example](https://raw.githubusercontent.com/MiniMax-AI/MiniMax-Code-Plugins/main/examples/hello-mcode-hooks/README.md), [preview hook layout](https://raw.githubusercontent.com/MiniMax-AI/MiniMax-Code-Plugins/main/examples/hello-mcode-hooks/io.minimax.mcode/hooks/hooks.json)

Muse's installed Ubuntu CLI returned **1.1.1 (1.1.1-R2514.1)**. Its help exposes `serve` (MSP), `schema`, `trace`, `export`, `--no-session-log`, and a deterministic `echo` provider. Those are feasible seams for unpaid lifecycle experiments; none establishes a hook config. Buildmesh launches with `--disable-approval`, which does not establish whether questions or non-tool prompts can occur. Existing [interop research](windows-wsl-harness-interop.md) records the log/index paths. [Meta documentation entry point](https://dev.meta.ai/docs/muse-code)

Freebuff's first-party repository identifies its CLI implementation, but this investigation found no external hook registration contract there. A source-backed event/log adapter remains work, not something to infer from terminal silence. [Freebuff source/spec](https://github.com/CodebuffAI/freebuff/blob/main/freebuff/SPEC.md)

DeepSeek's first-party launcher documentation explicitly makes SDK/ACP/headless/web profiles distinct, with profile-specific arguments and plugins. Implementing an ACP or SDK integration is feasible but changes the adapter's execution contract. Its current gated capabilities correctly avoid promising an unvalidated profile. [DeepSeek launcher](https://raw.githubusercontent.com/deepseek-ai/deepseek-harness/master/apps/cli/README.md)

## Evidence needed before claiming parity

For every enabled integration, test completion, structured question, approval, answer/rejection, error, interruption, resumed session, multiple nodes sharing a directory, and stale callbacks after restart/new turn. Assert visible attention reason and durable state together. Exercise the actual hook runner on Windows and WSL, preserving unrelated user hook configuration. Configuration parsing and synthetic callback tests should be reported as such; they do not measure production delivery reliability. No percentage reliability claim is supported by this audit.

## Evidence collected in this change

- Rust attention-route suite: **78 passed** (classification, aliases,
  transcript degradation, OpenCode/Kimi correlation, stale turns).
- Full Rust library + integration suite: **3228 library tests, 8 autopilot
  security tests, 1 job-object test, and 9 PTY tests passed; 21 ignored** with
  serial execution.
- Native Kimi callback probe passed on Windows with stdin/body/header and
  bounded delivery assertions; the installed Kimi Code `doctor` acceptance
  check also passed (no model call).
- Frontend TypeScript/build check (`check.ps1 all-ts`): **3117 unit tests
  passed, 1 skipped; 63 integration tests passed; desktop/mobile build and
  bundle budget passed**; the 20 newly targeted listener/plugin tests pass.
  The existing fuzzy-search 5 ms performance assertion is load-sensitive in
  a fully parallel run and passes in isolation.
- These are contract, parser, configuration, and synthetic callback tests;
  they do not replace a sustained live delivery-rate benchmark across every
  installed harness/version.
