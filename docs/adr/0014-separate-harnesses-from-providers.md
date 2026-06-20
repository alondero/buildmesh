# 14. Separate Agent Harnesses from Model Providers

Status: accepted

## Context

Today, Buildmesh mixes the concept of the execution harness (e.g. standard `claude` / Claude Code, Codex, Antigravity, OpenCode, Terminal) with the model service provider (e.g. Anthropic, OpenAI, MiniMax, Kimi, Google).
This coupling has caused several problems:
1. **Redundant Code**: The adapters for `minimax` and `kimi` duplicate the standard `claude` adapter code because they simply spawn `claude` and redirect the backend URL and token via environment variables.
2. **Path Dependency & Platform Limits**: Different tools exist on different platforms (e.g. `codex` or `opencode`). Currently, agent harnesses are turned on/off statically based on the OS, rather than checking if they are installed/available on the user's machine.
3. **Usage Panel Bloat**: Buildmesh queries usage stats for all hardcoded providers in parallel on every settings page open, even if the user does not use or have credentials for some of them.

We need a flexible model where:
- A user can define custom compatible endpoints (e.g., DeepSeek, GLM) running through Claude Code or Codex.
- The UI options only reflect what is actually installed and configured on the machine.
- The usage stats panel only queries providers the user actually has active accounts for.

## Decision

1. **Explicit Separation**: Split the execution model into two distinct concepts:
   - **Model Provider (Account / Credentials)**: Stored API keys, custom base URLs, and balance/plan configurations (e.g., Anthropic, OpenAI, MiniMax, Kimi, DeepSeek).
   - **Agent Harness (Executor)**: The binary recipe (e.g., `claude`, `codex`, `agy`, `opencode`, `terminal`) that runs the agent.
2. **Configurable Harness Profiles**: Define profiles in `preferences.json` that pair a Harness with a Provider configuration (e.g., a "MiniMax via Claude" profile pairs the `claude` harness with the `minimax` API key and base URL). The UI launch dropdown renders these user-configured profiles.
3. **Eliminate Duplication**: Delete the `minimax.rs` and `kimi.rs` adapter modules. The standard `claude` adapter now dynamically handles arbitrary custom compatible endpoints by injecting the provider's API key and base URL during spawning.
4. **Startup Auto-Detection (Scan on Init)**: On the first application run, scan the system `PATH` and standard directories (like `~/.claude/` or `~/.codex/`) to detect installed tools and existing credential configurations. Only auto-populate and enable harnesses/providers that are actually detected on the host machine.
5. **No Backward Compatibility Burden**: Because the current user is the sole operator of this instance and doesn't require legacy migrations, we will perform a hard cutover to the new `preferences.json` structure directly.

## Trade-offs Considered

1. **Dynamic String IDs vs. Static Rust Enums**: Shifting database columns and Tauri IPC payloads from `enum Provider` to `String` (using the harness profile ID) reduces compile-time type-safety in Rust but is necessary to support user-added custom compatible APIs without recompiling the app.
2. **Scan-on-Init vs. Static Defaults**: Auto-detecting binaries on PATH makes the system much cleaner and prevents empty options that fail when clicked, at the cost of slight initialization logic on the very first startup.
