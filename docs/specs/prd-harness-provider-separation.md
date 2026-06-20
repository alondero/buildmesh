# PRD: Agent Harness and Model Provider Separation

Status: accepted
Triage Label: `needs-triage`

## Problem Statement

The user is facing configuration friction and visual clutter in Buildmesh due to the tight coupling between an execution agent harness (e.g., standard `claude` / Claude Code, Codex, Antigravity, OpenCode, Terminal) and the model service provider (e.g., Anthropic, OpenAI, MiniMax, Kimi, Google).

Specifically:
1. **Redundant Adapter Code**: Adding Claude-compatible endpoints (like MiniMax or Kimi via Claude Code) duplicates the standard `claude` adapter code since they are just shell redirects via env vars.
2. **Platform & Path Friction**: Harnesses are hardcoded on/off based on the host OS compile-time checks rather than runtime verification of binary existence, resulting in broken profiles if a binary like Codex is absent.
3. **Usage Stats Clutter**: The Accounts & Usage panel queries stats for all built-in providers simultaneously, leading to long timeouts or error badges for unconfigured accounts.
4. **Different Billing Types**: API-key-based payment models (like pay-as-you-go credits) are forced into plan-based percentage bars, which does not accurately reflect account status.

## Solution

1. **Decouple Harnesses and Providers**: Separate the local CLI executor (`Agent Harness`) from the credentials/endpoint configuration (`Model Provider`).
2. **Dynamic Harness Profiles**: Allow users to configure profiles in `preferences.json` pairing an executor with a provider (e.g. "MiniMax via Claude Code"). The UI will display these user-defined strings in the node launch menu and default provider selectors.
3. **Startup Auto-Detection (Scan-on-Init)**: On first run, scan the system `PATH` and standard paths for installed binaries (like `claude`, `codex`, `agy`). Only auto-populate and enable harnesses/providers that are detected on the machine.
4. **Dynamic Usage Stats**: Fetch and show usage statistics only for the active configured providers, formatting the card either as a quota percentage (for plans/subscriptions) or a credit balance (for pay-as-you-go API keys).
5. **Direct Cutover**: Since this is a single-user local environment, apply a hard cutover of the schema without legacy database migrations.

## User Stories

1. As a developer, I want a unified configuration page, so that I can configure my LLM accounts and executor profiles in one place.
2. As a developer, I want Buildmesh to automatically detect which agent binaries (like Claude Code, Codex, or Antigravity) are installed on my system at startup, so that I do not have to configure them manually.
3. As a developer, I want to name my harness configurations (e.g., "Claude (Subscription)" or "Kimi via Claude"), so that I can easily tell them apart in dropdown menus.
4. As a developer, I want the node spawning menu to only show the harness profiles that are active and installed on my system, so that I do not accidentally try to spawn non-existent agents.
5. As a developer, I want the default provider dropdown in Mesh Properties to display my custom harness profiles, so that I can select any compatible LLM endpoint as a default workspace runner.
6. As a developer, I want the Accounts & Usage panel to only show statistics for the providers I have configured, so that the settings screen loads quickly and without error states.
7. As a developer, I want my pay-as-you-go API keys (like MiniMax) to show credit balance remaining instead of a quota bar, so that I know exactly how much budget is left.
8. As a developer, I want to create custom Claude-compatible profiles by entering a custom base URL and API key, so that I can seamlessly run agents against new models like DeepSeek or GLM.
9. As a developer, I want the dynamic profiles to resolve instantly at spawn time, so that separating them does not introduce launch delays.

## Implementation Decisions

- **Backend Architecture & Modality**:
  - Separate `Provider` enum logic from the dynamic harness profile execution. Payloads and database columns (`meshes.default_provider`, `sessions.provider`) will shift from the enum to `String` identifiers.
  - Delete `minimax.rs` and `kimi.rs` adapter files. The standard `claude.rs` adapter will handle all compatible configurations dynamically by injecting `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` at spawn time.
  - Cache user profiles in-process in the Rust backend (`preferences.rs`) via the `CACHE` mutex to avoid disk I/O overhead during node launches.
- **Auto-Detection (Scan-on-Init)**:
  - On startup, check `which`/`where` for CLI binaries and look up configuration folders (`~/.claude/`, `~/.codex/`). Enable default profiles matching detected tools; always enable `Terminal`.
- **Usage API Updates**:
  - Update `ProviderUsage` to return either a list of `UsageWindow` (utilization percentage) or a new `BillingBalance` structure (credits remaining, spend, and currency) depending on the provider's billing mode.
  - Filter the parallel fetching pool in `get_all_provider_usage` to only query enabled model providers.

## Testing Decisions

- **Unit Tests**:
  - Verify that the path scanning logic correctly flags present vs. absent binaries.
  - Verify that environment variable injection maps correctly for custom compatible profiles during spawns.
  - Verify that `preferences.json` saves and loads dynamic lists of profiles and accounts.
- **Integration Tests**:
  - Mock API responses for usage stats to verify correct parsing of both `UsageWindow` and `BillingBalance` shapes.
- **Prior Art**:
  - Refer to existing tests in `src-tauri/src/preferences.rs` and `src-tauri/src/services/usage.rs`.

## Out of Scope

- Configuring OpenAI-compatible custom endpoints for Codex in V1 (only Claude Code compatible redirection is supported).
- Autopilot policy configuration resolving custom profiles without predefined mapping names.
- OAuth login flows inside the Buildmesh UI for custom providers (must use static API keys).

## Further Notes

- Resolving custom profiles is guaranteed to be sub-millisecond as all configurations are cached in-memory via `preferences.rs`'s `CACHE` mutex.
