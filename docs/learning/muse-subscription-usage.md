# Muse subscription usage

Verified against Muse Code 1.1.1 (1.1.1-R2514.1), running in Ubuntu WSL,
on 2026-09-11. The former local request counter was not connected to production
turns and could not represent account usage across sessions or weekly limits.

The installed binary retains `MintedKey`, `SubscriptionUsageSnapshot`,
`SubscriptionWindowSnapshot`, and `SubscriptionWeeklySnapshot` field names.
It also contains `response.subscription_usage` for streaming quota updates.
The account reconciliation endpoint supplies a fresh snapshot without running
a model or starting a CLI session:

- `POST https://api.meta.ai/muse-code/key`, JSON body `{}`.
- `Authorization: Bearer <providers.meta.access_token>` from Muse `auth.json`.
- `Content-Type: application/json`; no special User-Agent was needed.
- GET returns 405. POST with the user's existing OAuth credential returned 200.

Relevant response fields (account identifiers and credentials omitted):

```json
{
  "is_subs_active": true,
  "subs_tier_name": "Muse Code Everyday Usage",
  "subs_usage": {
    "window": {
      "used_percent": 94,
      "window_duration_mins": 300,
      "resets_at": 1789161315
    },
    "weekly": {
      "used_percent": 35,
      "resets_at": 1789344000
    }
  }
}
```

These match the user's `/usage` display: Current 94%, Weekly 35%.
Reset timestamps are Unix seconds, converted to RFC3339 for the existing UI's
local-time rendering. Percentages are already used percentages, not fractions.
Zero is a real reading. The server's subscription name is used verbatim.

The response also carries an API key and personal account fields. Buildmesh
deserializes only subscription fields, never logs the body, and does not save
or replace Muse's credentials. This is the CLI's key reconciliation call,
not a dedicated read-only quota endpoint. No model inference is requested.

Muse's launcher resolves `MUSE_AUTH_PATH`, then
`${XDG_CONFIG_HOME:-$HOME/.config}/muse/auth.json`. On Windows, resolve this
inside WSL's `sh -lc` environment, matching Buildmesh's launch wrapper, then
convert the guest path through the environment module before Windows I/O.
An environment API key or stored non-OAuth mechanism cannot supply subscription
quota. Authentication failures guide the user back to `muse login`; Buildmesh
does not attempt an undocumented refresh flow or fall back to request estimates.

The endpoint is an observed private contract. If its response changes, report
unavailable instead of guessing a quota. The normal provider cache applies;
its identity fingerprints the active OAuth credential, so changing credentials
does not reuse the preceding account's quota.

## Verification

Base: `9895cde6345468fc6c18a81b014e61f1bd772610`.

- `scripts/check.ps1 all -SerialRust`: passed desktop/mobile builds,
  3,107 frontend unit tests (one skipped), 63 frontend integration tests,
  the full Rust suite (3,192 library cases discovered plus integration tests),
  agent checks, and bundle budgets.
- After the final path checks and rendering regression were added:
  focused account-card/usage-tab tests passed 46 tests; `cargo test --lib muse
  -- --test-threads=1` passed 33 with three explicit live-test ignores.
- The explicitly invoked `live_muse_subscription` test passed against the
  user's WSL OAuth account from the Windows Rust adapter, returning 94%/35%.
- A debug build with `tauri.dev.conf.json` was exercised through real WebView2
  IPC. The rendered Muse card matched the backend's plan and both percentages.
  The Probe separator was set to 240px; card width was 209.33 CSS pixels with
  no horizontal overflow. Local screenshots were inspected. No baseline
  screenshot was captured.
- Strict `cargo clippy -- -D warnings` failed with 13 repository lint errors;
  no baseline lint run was performed. The test build also reports a duplicate
  test attribute in the existing OpenAI cost tests. These were not repaired
  as part of the Muse integration.
- Dev-profile startup reported `resize_agent: Agent not running` for restored
  nodes and an unavailable LAN bind address. No new panic entries appeared.
  The Muse usage checks passed; this is not a clean overall startup-log verdict.
- One intermediate focused compile raced a concurrent frontend rebuild and
  failed because `dist/mobile` was temporarily absent. Once assets were rebuilt,
  the focused Rust suite and live check passed. Build assets and Rust embedding
  must be sequenced when sharing a worktree.
