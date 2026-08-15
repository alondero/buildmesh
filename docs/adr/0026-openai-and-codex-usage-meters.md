# 26. OpenAI and Codex usage meters — auth discovery, wire contract, and degradation

Status: accepted

Dual-track usage metering for OpenAI and Codex: passive read-only discovery of Codex CLI subscription quotas (`~/.codex/auth.json` on host and WSL), monthly spend aggregation for Organization Admin API keys (`sk-admin-...`), and graceful degradation for standard Project API keys (`sk-proj-...`).

## Context

Buildmesh displays real-time usage meters for AI providers across two primary billing modes: subscription quota windows (e.g. 5-hour rolling windows with reset timers) and pay-as-you-go balances.

OpenAI and Codex present distinct metering architectures:
1. **Codex CLI (ChatGPT Subscription Quota):** Users authenticated via ChatGPT (Plus/Team/Pro) have access to a private rate-limit usage endpoint returning rolling quota consumption and reset timestamps. Authentication is stored locally on disk by the Codex CLI.
2. **OpenAI Platform API (Pay-As-You-Go Wallet / Spend):** OpenAI does not provide a public programmatic API for real-time prepaid credit balances using standard API keys. Monthly spend is accessible exclusively through the Organization Costs API, requiring an Organization Admin API Key (`sk-admin-...`). Standard Project API Keys (`sk-proj-...`) are restricted to inference endpoints and return `401`/`403` on organization billing routes.

## Decision

1. **Codex CLI Quota Discovery & Endpoint:**
   - **Endpoint:** `GET https://chatgpt.com/backend-api/wham/usage`
   - **Headers:** `Authorization: Bearer <access_token>` and optional `ChatGPT-Account-Id: <account_id>`.
   - **Auth Discovery Order:**
     1. Windows Host Override: `$CODEX_HOME/auth.json` (if `CODEX_HOME` is set and non-empty).
     2. Windows Host Standard: `%USERPROFILE%/.codex/auth.json` (or `%HOME%/.codex/auth.json`).
     3. WSL Fallback: If no host credentials exist and WSL is active, resolve the default WSL distribution via `env::get_default_wsl_distro()` and construct the host UNC path via `env::to_host_path("/home/<user>/.codex/auth.json")` (`\\wsl$\<distro>\home\<user>\.codex\auth.json`).
   - **Passive Read-Only Policy:** The fetcher is strictly read-only. It never writes to `auth.json` or invokes OAuth refresh grants. On HTTP 401/403 (token expiry), it marks `logged_in: false` and prompts the user to re-authenticate via the CLI (`Run 'codex' in terminal to log in`).
   - **Schema & DTO Parsing:** Deserializes `rate_limit.primary_window`, `secondary_window`, and `additional_rate_limits`. Parses `used_percent` (f64 consumption 0.0–100.0), `limit_window_seconds` (dynamic label resolution: 18,000s → `"5-hour"`, 604,800s → `"Weekly"`), and `reset_at` (Unix epoch seconds converted to RFC3339).

2. **OpenAI Platform API Spend & Degradation:**
   - **Endpoint:** `GET https://api.openai.com/v1/organization/costs?start_time=<month_start_epoch>&bucket_width=1d`
   - **Admin Keys (`sk-admin-...`):** Returns aggregated monthly spend in `BillingBalance { remaining: 0.0, monthly_spend: Some(spend), currency: "USD" }`.
   - **Standard Project Keys (`sk-proj-...`):** Gracefully degrades without failing provider login. Sets `balance: None`, `logged_in: true`, and surfaces `detail: Some("Monthly spend tracking requires an Organization Admin API Key (sk-admin-...)")`.
   - **Invalid / Revoked Keys:** Non-billing inference checks returning 401/403 set `logged_in: false`, `error: Some("Invalid API key")`.

3. **Wire Contract Normalization:**
   - `UsageWindow.used_percent` strictly represents consumption percentage (0.0 to 100.0) across all providers to preserve universal progress-bar invariants.
   - User-facing UI labels and tooltips display remaining percentages (e.g., `81.5% remaining · resets in 2h 33m`) derived from `100.0 - used_percent`.

## Consequences

- Buildmesh surfaces Codex subscription limits automatically across native Windows and WSL development environments without requiring manual credential entry.
- Expired Codex sessions cleanly direct the user to CLI re-auth without throwing uncaught UI errors or corrupting CLI auth files.
- OpenAI project API keys remain fully functional for agent execution without surfacing noisy billing errors, while organization admins gain visibility into monthly spend.
- The `ProviderUsage` and `UsageWindow` wire contracts remain backwards-compatible and consistent across all provider implementations.
