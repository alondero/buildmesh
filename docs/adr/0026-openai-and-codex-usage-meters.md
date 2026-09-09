# 26. OpenAI and Codex usage meters — auth discovery, wire contract, and degradation

Status: accepted

Dual-track usage metering for OpenAI and Codex: passive read-only discovery of Codex CLI subscription quotas (`~/.codex/auth.json` on host and WSL), monthly spend aggregation for Organization Admin API keys (`sk-admin-...`), and graceful degradation for standard Project API keys (`sk-proj-...`).

## Context

Buildmesh displays real-time usage meters for AI providers across two primary billing modes: subscription quota windows (e.g. 5-hour rolling windows with reset timers) and pay-as-you-go balances.

OpenAI and Codex present distinct metering architectures:
1. **Codex CLI (ChatGPT Subscription Quota):** Users authenticated via ChatGPT have access to a private rate-limit usage endpoint. Consumer plans (Plus/Pro and similar) typically return rolling quota windows. Business and Enterprise plans may omit those windows (`rate_limit: null`) and instead report a plan label, credit balance, and an individual monthly spend or credit control. Authentication is stored locally on disk by the Codex CLI.
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
   - **Schema & DTO Parsing:** Deserializes the provider-reported `plan_type` verbatim (unknown names are kept, not rejected). `rate_limit` is optional: a null or absent object is valid. When present, parses `primary_window`, `secondary_window`, and nested `additional_rate_limits`. Also maps top-level `additional_rate_limits` (named extra buckets), `credits` (optional remaining balance and unlimited flag), and `spend_control.individual_limit` (used/limit/remaining/percent/reset, often as decimal strings). Window `used_percent` is consumption 0.0–100.0; `limit_window_seconds` drives labels (18,000s → `"5-hour"`, 604,800s → `"Weekly"`); `reset_at` is Unix epoch seconds converted to RFC3339. Mixed replies may carry both rolling windows and budget meters.

2. **OpenAI Platform API Spend & Degradation:**
   - **Endpoint:** `GET https://api.openai.com/v1/organization/costs?start_time=<month_start_epoch>&bucket_width=1d`
   - **Admin Keys (`sk-admin-...`):** Returns aggregated monthly spend in `BillingBalance { remaining: 0.0, monthly_spend: Some(spend), currency: "USD" }`.
   - **Standard Project Keys (`sk-proj-...`):** Gracefully degrades without failing provider login. Sets `balance: None`, `logged_in: true`, and surfaces `detail: Some("Monthly spend tracking requires an Organization Admin API Key (sk-admin-...)")`.
   - **Invalid / Revoked Keys:** Non-billing inference checks returning 401/403 set `logged_in: false`, `error: Some("Invalid API key")`.

3. **Wire Contract Normalization:**
   - `UsageWindow.used_percent` strictly represents consumption percentage (0.0 to 100.0) across all providers to preserve universal progress-bar invariants.
   - User-facing UI labels and tooltips display remaining percentages (e.g., `81.5% remaining · resets in 2h 33m`) derived from `100.0 - used_percent`.

## Consequences

- Buildmesh surfaces Codex subscription limits, credit balances, and individual spend controls automatically across native Windows and WSL development environments without requiring manual credential entry. Consumer rolling windows continue to render; spend-only Business and Enterprise replies are accepted rather than treated as malformed.
- Expired Codex sessions cleanly direct the user to CLI re-auth without throwing uncaught UI errors or corrupting CLI auth files.
- OpenAI project API keys remain fully functional for agent execution without surfacing noisy billing errors, while organization admins gain visibility into monthly spend.
- The `ProviderUsage` and `UsageWindow` wire contracts remain backwards-compatible and consistent across all provider implementations.
