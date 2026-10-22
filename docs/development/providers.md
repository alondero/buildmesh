# Model providers, credentials, and spawn recipes

Status: current

## First-class Model Providers and the credential-per-row invariant

A **First-class Model Provider** is one Model Provider Buildmesh ships built-in
knowledge of — brand identity (icon, accent colour), billing model, and a Usage
Meter fetcher. Examples: Anthropic, MiniMax, Kimi (Moonshot). Rendered as one
card on the Providers page; polled by one Usage Meter fetcher. See CONTEXT.md
"First-class Model Provider" for the canonical definition.

**The invariant — one credential/billing identity per row.** Per CONTEXT.md,
"Usage follows the credential, not the pairing" — proxying one credential
through several harnesses is still one Usage Meter (a single Moonshot API key
used via Claude Code and via Codex is one wallet). A provider MAY legitimately
expose multiple Usage Meters (e.g. an Anthropic subscription *and* an API
wallet — different billing relationships, same brand). What is forbidden is
*duplicate rows for the same credential/billing identity* — that produces
two cards on the Providers page, two fetcher paths, and confusing UI.

**Usage Meter contract.** Rust owns the additive wire model in
`services/usage/types.rs`; `cargo test` generates its TypeScript consumers.
`ProviderUsage.windows` and `.balance` remain the compatibility path for
rolling percentage quotas and wallets. New adapters may additionally provide
a verbatim `plan` label and explicit `meters`: `metered` carries a
`UsageAmount` (used, optional limit/remaining, unit, optional percentage and
reset), `no_individual_limit` carries the same amount without inventing a
limit, and `unlimited`, `managed_externally`, and `unavailable` distinguish
valid non-percentage states. The UI renders zero as zero; only an absent legacy
percentage or an explicit unavailable state is labelled "Unavailable". The Codex
adapter maps ChatGPT `plan_type`, rolling windows, top-level extra rate limits,
credit balance, and `spend_control.individual_limit` into that contract; a null
`rate_limit` is a valid Business/Enterprise snapshot rather than a parse error.
Muse Code (`muse-code`) reads the harness OAuth credential and calls Meta's
`POST /muse-code/key` reconciliation endpoint. Its `subs_usage.window` and
`subs_usage.weekly` supply percentages and Unix-second reset times; the plan
label comes from `subs_tier_name`. Windows tries the native login first —
one resolution in launcher precedence (`MUSE_AUTH_PATH`, then absolute
`XDG_CONFIG_HOME`, then `%USERPROFILE%` with `HOME` as a last resort) —
then the WSL guest credential path, using the first file that holds an
OAuth login.
Never derive remaining allowance from local requests or MSP token/context
events, and never fold it into a Meta Model API pay-as-you-go wallet.

**Observed Session Telemetry is not a Usage Meter.** Muse MSP
`session/tokenUsage` and `session/contextUsage` notifications are ingested by
`agent/provider/muse/telemetry.rs`, stored per Agent Node, and surfaced on
Node Detail plus the Node Digest `observed_session_telemetry` field. The
payload is labelled `kind: observed_session_telemetry`. Do not route it
through `UsageAdapter`, `get_provider_meters`, or the Usage probe tab, and
do not derive remaining quota, reset time, or dollar spend from it. Cache
read/write counters stay separate from counted-once `prompt_tokens`. The
Muse lifecycle task should call `telemetry::ingest_line` for every
`MspTransport::events()` notification once that transport exists.

**Usage Meter misses coalesce per credential identity (#2021).** The 5-minute TTL cache in `services/usage/cache.rs` collapses *sequential* reads; it cannot collapse a burst of *concurrent* cold reads. `cached_outcome_and_usage` therefore joins a per-`(provider, identity)` single-flight (`join_flight`) on a miss: the first caller becomes the Leader and performs the vendor fetch, concurrent callers for the same identity become Followers and wait on a `Condvar` for the leader's `(UsageOutcome, ProviderUsage)` pair. The adapter seam is synchronous, so the followers are the `spawn_blocking` threads the command already uses. No lock is held across the vendor round-trip, and a different provider *or a different credential for the same provider* gets its own slot, so unrelated providers are never serialized behind the slowest one. `force_refresh` skips the TTL read but still joins the flight: the leader is making a live request, so its result is fresh by construction (two simultaneous Refresh presses collapse to one vendor call). Followers receive the leader's **outcome**, not a value re-derived from the cached wire triple, so the `NoCredential`/`Rejected` distinction the gate depends on survives coalescing. `LeaderGuard`'s `Drop` releases the slot and publishes an `Unavailable` fallback on every exit path, including a panic, so a failed fetch stays visible and no caller can block forever. The pattern mirrors `services::gh_auth_cache.rs` (`misses: AtomicU64`).

**Usage page links.** Each `UsageAdapter` owns an optional `usage_page` for
its billing identity: a `UsagePage` URL and label that distinguish dashboards
from console, guide, or subscription fallbacks. `assemble_meters` attaches it as
additive `ProviderMeters.usagePage`, including disabled, failed, and remembered readings;
URLs are metadata, not stored in the reading caches. The catalog omits the link
when a reading reports `managed_externally`, and unknown providers have none.
The Usage panel renders it through `SafeLink` to open the system browser, as
an icon-only `↗` link in the row's header beside the provider name rather than a
labelled link of its own, so a meter's height does not grow with the destination.
The label travels in the `ariaLabel` and hover `title` instead of visible text.

The icon-only form suits a row that already shows the provider name, but it
shrinks the hit target and drops the visible cue the labelled call sites carry
(`PrPill`, the Git Issues / PRs tab headers); prefer a labelled link where the
surrounding row does not already name the destination.
Provider destinations and first-party evidence are recorded in
[the usage-page research](../research/provider-usage-pages.md).

**Usage cache identity.** The five-minute cache is keyed by provider plus an
opaque account/authentication-source fingerprint selected through the
`UsageAdapter` seam. Keyed adapters receive a process-salted SHA-256
credential fingerprint automatically; native adapters override the identity
when their provider can select OAuth, cloud, workspace, or other credential
sources. Fingerprints keep tokens, keys, account identifiers, and credential
paths out of cache keys and logs. A repeated identity keeps the existing TTL
hit; changing account or auth source starts a distinct entry.

**Usage Meter last-known fallback.** A second, *durable* tier
(`services/usage/last_known.rs`) keeps the last reading each provider actually
reported in `<app_data_dir>/usage_last_known.json`, for seven days from the last
successful fetch — it survives restarts, unlike the five-minute in-process cache.
When a fetch cannot produce a reading because no usable credential is available
(a harness not signed into yet today, an expired native token),
`assemble_meters` serves that remembered reading instead of hiding the row and
stamps `ProviderMeters.cachedAt` (epoch seconds, `null` for a live fetch) so the
Usage tab can render "Last known value · …" plus a `Cached` header badge with
the fetch instant on hover. A transient failure (rate limit, transport error, or
a provider answering without usable quota) falls back the same way while a
reading is remembered. It is keyed by **provider id**, not
the identity fingerprint (which is process-salted, and for Muse *is* the rotating
access token). It never replaces a live reading or a
rejected-key prompt; with nothing remembered the row is hidden (or errors) as before. See
[ADR-0037](../adr/0037-usage-last-known-fallback.md).

**Antigravity (`agy`) quota request version gate.** The `agy` meter calls
`POST daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary`
with the Antigravity CLI User-Agent, and the API gates the response on the CLI
version that User-Agent advertises. Below cli `1.2.0` it still answers
`HTTP 200` but strips the "Gemini Models" (weekly + 5-hour) group, so both
Gemini meters silently disappear — an old pin is a blank meter, not an error.
`AGY_USER_AGENT` (`services/usage.rs`) pins the current published CLI version
(`agy --version`), verified live 2026-10-16 at `1.4.0`: `1.1.5` returned only
"Claude and GPT models", `1.2.0` added "Gemini Models", and `1.4.0` also returned
the "Gemini 4 Argon" group.
`agy_user_agent_meets_gemini_quota_version_gate` fails if the pin drifts back
below `1.2.0`, and when a response carries no Gemini group at all
`parse_agy_quota_summary` attaches a `detail` so a re-gate surfaces instead of
blanking.

**Claude Code authentication source.** The Anthropic meter follows Claude's
documented credential precedence rather than always reading
`~/.claude/.credentials.json`. Cloud flags (`CLAUDE_CODE_USE_BEDROCK`,
`CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_USE_FOUNDRY`) from the process
environment or the user `settings.json` `env` block report
`managed_externally` for AWS Bedrock, Google Vertex AI, or Microsoft Foundry
and must not present a dormant OAuth login as active. Environment API keys,
bearer tokens, and `apiKeyHelper` outrank stored OAuth. `CLAUDE_CODE_OAUTH_TOKEN`
uses that token for the request and derives plan from usage-body plan fields or
the token's own `/api/oauth/profile`, never from a dormant local login.
**Exception — `claude setup-token`:** those tokens are model-request-only
(`user:inference`) and commonly lack `user:profile`. Anthropic's `/usage` and
`/profile` endpoints then return a scope/`permission_error` rather than a
usable plan. Buildmesh keeps the account logged in, surfaces an explicit
scope limitation (not "login expired"), and does not invent a plan name.
Full `/login` OAuth (or a token that includes `user:profile`) is required for
Enterprise plan + spend together. This is an intentional exception to the
"Enterprise OAuth accounts show plan and spend" acceptance criterion for
inference-only env tokens. Named Anthropic profiles
are mode-aware: `user_oauth` uses the profile credential for usage, while
`oidc_federation` (named, active, or env-configured) reports
`managed_externally`. Env federation requires the full WIF set (rule, org,
service account, and identity token/`_FILE`); a partial pair does not suppress
stored OAuth. A set-but-empty `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN`
still occupies that credential slot. An active `user_oauth` profile ranks
below a *working* `/login` credential (expired tokens and fetch 401/403 fall
through / retry) and above a missing login. `user:profile` scope failures are HTTP 403 with the explicit scope message:
env tokens mention `setup-token`, Claude Code `/login` credentials guide
`/login`, and named `ANTHROPIC_PROFILE` credentials guide
`ant auth login --profile <actual-name>` (because `/login` cannot repair that
higher-priority profile). Non-scope 401/403 auth failures are likewise
origin-aware: env tokens guide refreshing or unsetting
`CLAUDE_CODE_OAUTH_TOKEN`, profiles guide `ant auth login --profile
<actual-name>`, and `/login` store credentials guide `/login`. HTTP 401
remains an expired/revoked credential even if the body mentions scopes. Native OAuth reads the platform store (macOS
Keychain service `Claude Code-credentials`, suffixed from `CLAUDE_CONFIG_DIR`,
with `.credentials.json` as fallback when Keychain is missing or unusable) and
queries `GET /api/oauth/usage` directly — never by spawning the Claude CLI.
Consumer plans keep five-hour and seven-day windows; Enterprise prefers the
`spend` object and falls back to `extra_usage`.

**The Spawn Menu is where harness↔provider pairings live.** Its backend list
contains one Spawn Option per **stored** `(harness, provider)` pairing as the
composite id `<harness>:<provider>` (e.g. `claude:kimi`). Pairings are *not*
rows in `BUILTIN_PROVIDER_ACCOUNTS` — they live in
`AppPreferences::provider_pairings` and `effective_pairings` returns stored
rows only (ADR-0025: no auto-derived Claude pairing on key alone). Endpoint
URL + model tiers live on the pairing (Harnesses page), not the account.
The desktop spawn picker renders harness parents and puts their saved Launch
Configurations in each harness submenu — only user-saved recipes appear, so a
fresh install shows none. The backend also retains
the pairing rows for selectors that still choose a provider route directly.

**The registries are independent.** The brand string may coincide across
namespaces, but each registry is the single source of its own kind:

`BuiltInProviderAccount::self_auth` describes the account's credential
ownership: `true` means its CLI manages authentication; `false` means a keyed
account, including Usage Meter-only providers such as OpenAI that have no
first-class inference surface. The keyed Moonshot / Kimi account therefore
has `self_auth: false`, while the Kimi Code CLI Agent Harness manages its own
authentication in the separate harness namespace.

| Registry | What it carries | Example for Kimi |
|---|---|---|
| `BUILTIN_PROVIDER_ACCOUNTS` (`src-tauri/src/preferences/resolver/catalog.rs`) | One row per credential/billing identity. Self-auth rows always appear via `default_provider_accounts`; keyed first-class (`self_auth: false`) are catalog-only until added (`keyed_first_class_catalog`) — credentials + Usage Meter only. | `id: "kimi", self_auth: false` (endpoint on pairing / `first_class_surfaces`) |
| `HarnessProfile` + `Provider::Kimi` enum variant + `KIMI` adapter | The Kimi Code CLI Agent Harness — uses `~/.kimi-code/config.toml` by default for its own auth; Buildmesh doesn't manage the credential. | `HarnessProfile { id: "kimi", harness: "kimi", binaries: &["kimi"] }` + `KimiAdapter` |
| `src-tauri/src/services/usage/catalog.rs` | One row per fetchable Usage Meter: provider id, native-harness visibility gate or account-key lookup, and fetch adapter. Add a meter here instead of creating parallel lists or dispatch matches in `commands/usage.rs`. | `UsageMeterDefinition::keyed("kimi", usage::kimi_usage)` |

The string `"kimi"` appearing in both is fine because the namespaces are
different (`ProviderAccount.id` vs `HarnessProfile.id` / `Provider` enum).
What is **never** fine is two rows in `BUILTIN_PROVIDER_ACCOUNTS` for the same
credential — that produces two cards, two fetcher paths, and confusing UI.

**Pinned by tests** (regression net):
- `builtin_provider_accounts_have_no_via_substring_in_id` (`src-tauri/src/preferences/tests/catalog_tests.rs`) — any id with `"via"` is a pairing shorthand and must be expressed as a composite Spawn Option (`claude:kimi`), not as a separate row. A mechanical guard against the specific class of bug PR #1044 introduced.
- `kimi_via_claude_id_does_not_exist_in_default_provider_accounts` — the literal dual-id bug.
- `kimi_is_first_class_claude_compatible_with_moonshot_endpoint` — catalog + `first_class_surfaces` shape (not in defaults).
- `provider_accounts_migrates_stored_kimi_via_claude_into_first_class_kimi` — one-time migration for users who picked up PR #1044.
- `loading_preferences_never_persists_builtin_defaults_as_stored_accounts` / `loading_a_legacy_kimi_companion_file_leaves_it_byte_identical` (`src-tauri/src/preferences/tests/accounts_tests.rs`) — a load must not turn code-defined defaults into stored accounts (issue #2411).
- `default_provider_accounts_are_self_auth_only` / `effective_pairings_stored_only_no_auto_derive` / `migrate_legacy_account_endpoint_into_claude_pairing` — ADR-0025.

**Don't.** Do not add a `kimi-via-claude`-style companion row when restoring
or re-introducing a First-class Model Provider. Keep it in the keyed catalog
(`self_auth: false`) and let the Harnesses page attach pairings explicitly.
If a credential migration is needed (e.g. a user already stored a key against
the companion id), carry it over in a one-time read migration that persists
back to `preferences.json` — don't leave stale data.

**One-shot migration flag.** A read-migration that *auto-derives* state
(ADR-0025: pairing rows for legacy keyed accounts with no Claude attach) must
be gated on a persisted boolean so it runs exactly once per install. Pattern
in `preferences.rs`: `ad0025_account_pairings_migrated: bool` on
`AppPreferences` (`#[serde(default)]` so older installs load as `false`),
set inside `migrate_prefs_json`, then a re-deserialise gate (`serde_json::from_value`
returning `Err` ⇒ keep the on-disk file intact rather than `unwrap_or_default()`,
which previously overwrote a partially-unknown prefs file with defaults — a
silent data-loss path).

**A read is not a licence to persist defaults.** `provider_accounts()` resolves
the effective list as `default_provider_accounts()` merged with the *stored*
`provider_accounts`, and the two lists must not be confused. Anything that
migrates the stored list (`migrate_kimi_companion` in
`src-tauri/src/preferences/resolver/accounts.rs`) therefore runs **before** the
merge and persists only the migrated stored list: a read that wrote the merged
list back stored every built-in default as an account the user never chose
(issue #2411). A legacy file on disk takes a different path — `migrate_prefs_json`
already folds the `kimi-via-claude` companion during classification, before
deserialization, so that migration needs no write-back of its own. (A load can
still rewrite the file for an unrelated reason: the #830 plaintext-key scrub.)

**A read failure is not a licence to write defaults.** `preferences.json` holds
credentials, so an unreadable file must never be replaced by
`AppPreferences::default()`. `preferences::storage` owns that rule: it classifies
the file into `LoadState::{Missing, Healthy, Corrupt}` on every read, publishes
only `Missing` and `Healthy` to the writable cache, and re-checks the file on
every write rather than latching a flag that can disagree with the disk. A
corrupt read still *serves* defaults so read-only callers (spawn routing, the
circuit classifier, the usage panel) keep working; the defaults simply never
reach the disk. `preferences::recovery` owns the bytes: `classify` is pure and
returns a content-free `CorruptionInfo` (a corrupt file may still hold API
keys, so no diagnostic may echo a value), every successful write refreshes an
owner-only `preferences.json.bak`, and `restore_backup` / `reset_to_defaults`
archive the current bytes before replacing them. Surface the state as a
*successful* `get_preferences_health` call — never as an `Err` string the UI
has to pattern-match, and never as a `failed` resource status, because the
read itself did succeed.
**The Settings Harnesses pane does not depend on the Spawn Menu — and neither
does its retry.** The attach picker ("Add proxied provider") resolves its whole
`harness_id → compatible accounts` map in one backend call,
`preferences::compatible_providers_by_harness` — keyed by the union of the
effective harness profiles, the harness ids named by stored pairings, and the
harness half of every saved Launch Configuration's spawn option, so it covers
every row the pane can render. `loadPairings` therefore takes no providers list,
and the modal's mount fan-out loads pairings alongside every other resource
(issue #1935). Before that, pairings took its harness ids from `list_providers`
and issued one `compatible_providers_for_harness` per harness, which both cost N
round trips and made the pane open in `providers + pairings` — the Codex install
probe behind `list_providers` could take seconds. Don't reintroduce the
dependency.

`retryResource` in `useSettingsResources` takes no cross-resource preconditions:
every loader reads what it needs, so a retry is just the loader again. The
`retryResource('pairings')` boundary check from #1534 round 4 ("Awaiting providers
list — retry providers first") is **gone** and must not come back. It required a
providers list that no call site ever passed, so Retry on a failed pairings load
re-read nothing and replaced the real error with an unrecoverable message — and
since providers had usually succeeded, the user had no providers banner to be
pointed at and could only recover by reopening the modal. Its original
justification was the provider-menu dependency, so removing that dependency
removed it. When providers genuinely fails, the Harnesses pane renders the
providers banner ahead of the pairings one; that ordering is the affordance the
check was standing in for.

## Credentials (Windows Credential Manager)

The Buildmesh-managed OAuth secrets live in Windows Credential Manager under `CRED_TYPE_GENERIC` (the catch-all "store arbitrary bytes" type — domain credentials are a separate `CRED_TYPE_DOMAIN_*` family and we never use them). FFI is hand-rolled over `advapi32!CredReadW` / `CredWriteW` / `CredDeleteW` rather than `windows-sys`, matching the project's "minimal-FFI for the two-or-three functions we actually call" convention (also used by `sandbox::restricted_token`).

- **Surface** (`src-tauri/src/services/windows_cred.rs`):
  - `read(target: &str) -> Result<Vec<u8>, UsageError>` — missing credential collapses to `NoCredential(target)`; empty blob bytes round-trip as `Vec::new()` so a higher-level parser can decide what "empty" means.
  - `write(target: &str, blob: &[u8]) -> Result<(), UsageError>` — upsert via `CredWriteW` with `CRED_PERSIST_LOCAL_MACHINE` (persists across reboots, local-user-scoped; never `CRED_PERSIST_SESSION` — OAuth tokens need to survive logoff — and never `CRED_PERSIST_ENTERPRISE`, which requires domain policy we don't ship). `UserName` is set to the same string as the target so Credential Manager's detail view is manageable.
  - `delete(target: &str) -> Result<(), UsageError>` — **idempotent**: a `FALSE` return followed by `GetLastError() == ERROR_NOT_FOUND (1168)` collapses to `Ok(())` so the Settings "Sign out" affordance never errors on a no-op. Any other Windows failure surfaces as `Shape(target, GetLastError)`.
  - `cfg(windows)` only. Non-Windows callers see `NoCredential(...)` from their `cfg`-gated helpers instead.

- **Known targets** (extend-only — never delete from this list without a migration ticket):
  - `gemini:antigravity` — written by older Antigravity CLIs, read-only here. Current CLI (1.2+) refreshes `<agy_dir>/antigravity-oauth-token` instead and leaves this target stale; the Usage Meter prefers the file, then this keyring, and retries the alternate source on HTTP 401/403.
  - `buildmesh:<profile>:provider-account:<account id>` and `buildmesh:<profile>:minimax-api-key` — a provider account's API key and the deprecated flat MiniMax key, written by `preferences::secrets` (issue #830). The blob is the key as UTF-8 text. `<profile>` is the app-data directory's leaf, i.e. the bundle identifier, so the stable and dev builds (same OS user, one vault) never read or delete each other's keys. Never rename either segment without a migration: the entry name is the only link from an account to its key.
  - `opencode:console` — written by Buildmesh for the OpenCode Go OAuth dance (issue #956). Persisted blob is JSON `{ access_token, workspace_id, refresh_token, expires_at, server_id }` (RFC-3339 string for `expires_at`, mirroring the original #957 fixture so the live probe still parses; the `server_id` field is the SolidStart deployment id captured into the `X-Server-Id` header).

- **Provider API keys** (`src-tauri/src/preferences/secrets.rs`, issue #830) are ordinary `api_key` / `minimax_api_key` fields in memory and never fields on disk. `storage::write_to_disk` calls `secrets::externalize` (store each key, blank the ones the store accepted, delete the entry of a cleared key or removed account); `storage::read_state` calls `secrets::hydrate_json` through `recovery::classify_with` **before** the read-time migrations, because ADR-0025's migration creates a pairing only for an account that has a key. Rules a change must keep: a key leaves the file only after the store accepted it (no store means it stays in the file, never lost); a key present in the file wins over the stored one (hand edit, restored backup); a file from an older build is scrubbed in place on first load (`secrets::scrub_file`: only the secret fields change, no shape migration and no fabricated backup, preserving the read-never-persists rule), and so is its `.bak`; a recovery reset deletes nothing; a full-fidelity bundle (`state_recovery::build_bundle`) inlines the keys back through `secrets::with_keys_inlined`. Non-Windows builds have no store, so keys stay in the file. Tests use a per-thread in-memory vault (`secrets::test_support`), so they never touch the real Credential Manager; the OS wrapper has its own round-trip test that skips where Credential Manager is unreachable.

- **In-app notice when the fallback is active** (issue #2154). `preferences::secrets::account_ids_with_keys_on_disk` reads the **raw files** — `preferences.json` and its `.bak` — and returns the account ids whose `api_key` is still a non-empty string there; `resolver::provider_accounts_with_keys_in_preferences` re-exports it and `commands::preferences::get_provider_accounts_with_preferences_keys` puts it on the wire. `AccountsPane` passes each id to `AccountCard` as `keyInPreferences`, which renders a warning `InfoTip` (an amber "!" with the message on hover) beside the account name, outside the row's expander. Two rules a change must keep: it reads the files rather than the hydrated `AppPreferences`, because after `hydrate` a key from the file is indistinguishable from one from the store — that is the whole question; and it is deliberately **not** a field on `ProviderAccount`, which is both the persisted shape and the wire type, so a storage-location flag there would be written into the very file being reported on. The deprecated flat `minimax_api_key` is reported against the `minimax` account only when that account has no key of its own, matching the fold `resolver::accounts::provider_accounts` applies.

- **Operator commands** for diagnosing drift:
  - `cmdkey /list | findstr antigravity` — confirm the legacy `gemini:antigravity` keyring target is present (metadata only; does not dump the blob).
  - `Get-Content $env:USERPROFILE\.gemini\antigravity-cli\antigravity-oauth-token` (or `$env:GEMINI_HOME\antigravity-cli\antigravity-oauth-token`) — inspect the live CLI oauth file; redact before pasting. Compare `token.expiry` against the keyring blob when the Usage Probe drops Antigravity.
  - `cmdkey /list:buildmesh:*` — list the stored provider API key entries for every profile (metadata only; the entry name carries the profile and account id, never the key).
  - `cmdkey /list:opencode:console` — read the current blob's user/credential metadata without dumping bytes.
  - `cmdkey /list:buildmesh-test-*` — find any leftover test credentials from a failing test that didn't clean up. Each unit test uses a uuid-suffixed target name so collisions are vanishingly rare.

- **Pitfalls** — each caught by an iteration of real bugs:
  1. **`GetLastError` is mandatory after a `CredDeleteW` FALSE return.** Microsoft conflates "didn't exist" with "real failure" by returning FALSE for both, so a TRUE-only check would shadow the idempotent revoke the Settings UI relies on.
  2. **`from_raw_parts` requires non-null even for length 0.** Guard with `if cred.credential_blob.is_null() || cred.credential_blob_size == 0` to avoid UB on a freshly-written credential whose blob pointer hasn't been allocated.
  3. **`CRED_PERSIST_SESSION` is wrong for OAuth tokens.** A credential with this flag is gone after logoff — fine for session-only secrets (the kind cwrap adapters carry), wrong for a long-lived refresh token.
  4. **The Rust blob format is implicit.** Buildmesh's parser (`services::opencode_oauth::parse_opencode_console_full_credential`) currently shapes a 5-field blob: `access_token` + `workspace_id` + `refresh_token` + `expires_at` + `server_id`. The first two are required for the live probe (see `parse_opencode_console_full_credential`); the last three ride along for the refresh path and the `X-Server-Id` header fallback. Any future extension that adds a sixth field must update both the writer (`services::opencode_oauth::persist_token_response`) AND the parsers atomically, or the live probe will silently drop the field (serde defaults — `skip_serializing_if = "Option::is_none"` on the writer + `#[serde(default)]` on the reader means both sides tolerate missing keys but never notice renamed ones). `parse_full_credential_round_trips_all_five_fields` pins the contract; CI's `git diff --exit-code src/types/generated/` is the wire-shape gate, but not the Rust-side wire-shape gate — keep both directories in sync by hand until both ts-rs exports and Rust struct shape are auto-derived.

## Agent runtime PATH on macOS and Linux

Finder and Dock launches can inherit a minimal PATH even when discovery finds
an agent in an NVM version directory. An absolute agent path alone does not
help a `#!/usr/bin/env node` entrypoint find its runtime.

For native Unix PTY and background agent launches, `agent::spawn_environment`
prepends the resolved executable's directory to the child's PATH. This lets
the agent use its sibling runtime, including the Node version that owns an NVM
install. Inherited PATH entries remain after it; the app's own environment is
unchanged. Bare command names and relative executable paths retain their
inherited PATH. Windows and cross-runtime launches keep their existing
environment handling.

## Agent Spawning on Windows

The shell a provider spawns through is **adapter-owned, and deliberately not
enumerated here**. Each harness adapter's `spawn_recipe`
(`src-tauri/src/agent/provider/adapters/<id>.rs`) sets `SpawnRecipe::windows_shell`,
and `spawn_environment::wrap` consumes that value. macOS and Linux always spawn
`Direct`, so only the Windows arm of a platform match can differ.

The common arms have one table, `provider::shell_for` (`Cmd` on Windows, `Direct`
elsewhere), which the `.cmd`-shim adapters call instead of restating the match
— seven identical copies existed before issue #2235. An adapter with a real
reason to differ keeps its own function and documents it: Codex's `shell_for`
returns `PowerShell`, and `anthropic` declares no value at all (see below).

The three values exist for three structural reasons, which is the part worth
remembering:

- `Cmd` — the provider binary is an npm `.cmd` shim, which `CreateProcess`
  cannot execute directly. This is the common case, and it is why adding a
  `.cmd`-shimmed harness needs no other change.
- `PowerShell` — the provider's own binary needs ANSI output to propagate.
  This is the rare case: find the current holders with
  `rg -n "WindowsShell::PowerShell" src-tauri/src/agent/provider`.
- `Direct` — the provider ships a native executable. The plain terminal
  harness is one of these: it *is* the terminal, so there is nothing to wrap.

A Windows-runtime node whose directory lives in WSL spawns with a UNC current
directory, and **cmd.exe rejects one** — it silently starts in `C:\Windows`
instead. `Cmd` answers that with its own `pushd`, which maps the share for the
lifetime of that shell. `PowerShell` has the same problem for a different
reason: it cannot run a `.cmd`/`.bat` shim at all, so it delegates to
`cmd.exe /c` and lands straight back on the rejection. So when the spawn is a
batch shim in a UNC directory, the PowerShell arm runs *inside* cmd's `pushd`:
`cmd.exe /d /c <temp-batch.cmd>`, where the batch `pushd`s the directory quoted
in the file, with `%` written as `%%` to prevent batch expansion. A short
`-EncodedCommand` loader reads a companion UTF-8 PowerShell script through .NET
and invokes it as a script block. The original script retains the single-quoted
harness arguments, including multiline and Unicode prompts. Literal quoting
doubles ASCII and smart apostrophes because PowerShell recognizes both as
string delimiters. Using a script block avoids introducing a `-File` execution
policy requirement.

The full encoded spawn script must not occupy a batch line: UTF-16LE plus
Base64 expands ordinary text to about 2.7 times its length, so an otherwise
valid launch can cross cmd's 8,191-character batch-line limit. The mapped path
budgets the loader's line and the actual harness command separately. A batch
shim's child command still has cmd's 8,191-character limit; direct processes
and unmapped encoded PowerShell launches retain the Windows process budget.
The temporary `.cmd` and `.ps1` files share a random stem and are reclaimed by
the stale-file sweep. The loader sets `$ErrorActionPreference = 'Stop'` for the
read and invocation, including the companion script, so PowerShell errors
abort the launch. Native child exit codes propagate through the loader.

The decision is taken from the command and never by probing the filesystem: on
the `WindowsInterop` arm Buildmesh runs on the Linux host, so a Windows path is
not on a filesystem it can see and a probe would silently decline. A `.exe` or a
bare stem is launched by PowerShell itself and never reaches cmd.exe, so it
needs no mapping — which also leaves a drive-letter directory, a `.exe`, and a
bare stem on today's behaviour, and keeps any host PowerShell execution policy
out of the picture entirely.

Two mechanical traps in that mapping, both learned the hard way:

- **The directory must never appear on a command line.** portable-pty quotes an
  argument only when it contains whitespace or a quote, so `&`, `|`, `<`, `>`,
  `^`, `(`, `)` and `%` in a node's directory reach cmd.exe bare, where cmd
  treats them as operators — a node in `R&D` would run whatever followed the
  ampersand. Quoting inside the argument is not available either, because
  portable-pty escapes a `"` inside an argument, so a pre-quoted path arrives as
  `"\"<unc>\""` and `pushd` rejects the line with *The filename, directory name,
  or volume label syntax is incorrect.* The mapping therefore runs from a
  one-shot batch file with the directory quoted and percent signs doubled, so
  the command line carries the batch file's own path. The `Cmd` arm still
  passes its UNC path as a plain argument and keeps that exposure; see
  [`needs_unc_pushd`] in the module for why it is out of scope for this change.

On `WindowsInterop` both temporary files need a **Windows-visible** directory
(`std::env::temp_dir()` on the Linux host is the guest's `/tmp`). The batch
carries the node directory itself, so mapping does not depend on forwarding a
directory variable through `WSLENV`. A failed `pushd` aborts before the harness
runs.

The same split governs the batch file's own path: written through the host
spelling of a Windows directory (`/mnt/c/...`), because that is how Linux opens
it, but handed to `cmd.exe` in the Windows spelling (`C:\...`), because that is
how Windows resolves it. `windows_cli_home` returns the host spelling — correct
for opening a file, wrong for telling Windows where it is.
- **Only a mapped spawn drops its working directory.** Handing cmd.exe the UNC
  anyway prints *Defaulting to Windows directory* and makes a correct spawn look
  broken. But `Direct` never establishes a location, and PowerShell running a
  `.exe` or bare stem neither `pushd`s nor `Set-Location`s — suppressing the cwd
  for those arms makes `portable-pty` fall back to `USERPROFILE` and opens the
  agent in the home folder. All three sites read one decision,
  `maps_directory_with_pushd`, because they have to agree.

Read the adapter rather than trusting any list, including this one; a per-adapter
table rots silently as adapters are added, which is why the adapter is the only
place that decides and its own unit test is what pins the value.

The Claude-backed family does not declare its own value at all:
`adapters/anthropic.rs` delegates to `claude_direct_recipe(platform)` in
`src-tauri/src/agent/provider/mod.rs`, which pins `Direct`, so a new
Claude-backed adapter inherits the right shell instead of restating it.

Claude-backed launches pass initial prompts as `-- <prompt>`, which starts the
first interactive turn. `--prefill` only stages a draft awaiting Enter. The
shared adapter applies this to PR reviewers, PR and issue probe launches,
handovers to new nodes, and prompted creates, including saved provider routes.
The option terminator keeps a selection starting with `-` out of CLI option
parsing. Launches without an initial prompt still open an idle composer.

The adapter's `prefill_auto_submits` declares this submission behavior to
`initial_prompt_delivery`. Circuit launches use `AutoSubmit` to carry the
prompt in `SpawnIntent::Loop` without scheduling the staged-prefill Enter
watcher or injecting it again through the PTY. Harnesses that stage a draft
retain `Prefill` and its readiness watcher; prompts requiring live PTY delivery
retain `InjectAfterSpawn`.

### Launch permission modes are adapter-owned, not a Buildmesh policy

`spawn_recipe` is bare of approval flags (issue #2151): no adapter keeps a
hidden unattended argv. The effective permission mode — the stored
per-harness Settings default, else the harness's unattended default —
contributes the harness's own flag(s) in `default_prepare`
(`AgentProvider::permission_args`), spliced at the front of `base_args`
exactly where the base recipe used to carry them (behind a leading `resume`
subcommand token for `spawn_recipe_for_resume` adapters, so today's argv
order is preserved byte-for-byte). The descriptor the Settings card renders
(`permission_modes`, `default_permission_mode` on `HarnessCapabilities`)
comes from the same adapter methods, so the UI and the argv cannot disagree;
`capability_recipe_coherence` pins the agreement per adapter. The Spawn Menu
does not display the mode, although `effective_permission` is still stamped on
`ProviderInfo` rows. Harnesses with no flag expose no modes, and mcode's Full Access
is a singleton mode (a `config.yaml` pin, not argv). Static descriptors such
as the attention launch mode describe the unattended launch: a harness
switched to prompt mode in Settings can raise approval prompts. The orchestrator's
sandbox toggle and the attention-hook trust bypass are separate controls and
stay in their own layers.

The binary the shell invokes is the absolute path discovery resolved, not a
bare stem. A GUI-launched app (Finder/Dock on macOS, Start Menu on Windows)
inherits a restricted process `PATH` that omits user-managed directories
(`~/.local/bin`, Homebrew, Node manager shims, npm prefix bins), so spawning
`claude` by name fails even when the picker offered it. Detection
(`src-tauri/src/agent/detection.rs`) therefore searches those directories in
addition to `PATH` and records the resolved path on
`HarnessProfile.executable`. The launch router
(`agent::launch_routing::prepare` and `prepare_snapshot`) threads that path
onto the routing (both `Native` and `Environment`), and re-resolves the
adapter's recipe stem through the same enriched search at spawn time when the
profile carries none (config-dir-only installs, custom profiles).
`agent::spawn::command::build_spawn_command_prepared` consumes the routing's
resolved path without further lookup, keeping command composition pure and
unit-testable. On Windows only `PATHEXT` extensions resolve a bare stem, so an
unrunnable extensionless npm shim is never recorded. WSL guests and
Windows-interop spawns are exempt from host-side resolution: the guest login
shell (WSL) or the Windows side (interop) resolves the stem in its own
runtime, since a host-resolved Linux path is not valid input to PowerShell.

### Spawned agents never inherit the launching Claude Code session

A Claude Code session exports a set of markers into the environment of every
process it spawns, so each can tell it belongs to *that* session. When
Buildmesh itself was launched from such a session's shell — an agent running
`scripts\run-dev.ps1` for `/use`, `/verify` or `/verify-ui` — the app inherits
the whole set and would otherwise hand it straight back to the agents it spawns.
`CLAUDE_CODE_CHILD_SESSION` is the damaging one: the spawned Claude Code reads
it, prints `Transcript saving is off - inherited CLAUDE_CODE_CHILD_SESSION
marker`, writes no transcript, and every transcript consumer downstream (a
Circuit's first gate, session recovery) then parks on a file that will never
exist. The rest leak the launching session's identity and, for the messaging
token and socket, its credentials into another agent process.

`agent::spawn_environment` therefore scrubs them on **both** spawn paths, and
`CLAUDE_SESSION_MARKER_ENV_VARS` is that single list:

- `wrap` — every interactive PTY agent — clears them alongside
  `pty::strip_git_env_vars`.
- `background_command` — the pipe-based launches (session naming, circuit
  classifiers) — clears them too. Those build a `std::process::Command` rather
  than a `CommandBuilder`, so they are a second instance of the same leak, not
  a variant of the first.

Two things make this a list rather than a `CLAUDE*` prefix rule, and both are
load-bearing. First, Buildmesh sets `CLAUDE_*` variables **on purpose** —
`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` and
`CLAUDE_CODE_AUTO_COMPACT_WINDOW` for the MiniMax naming side-channel
(`provider_conf::minimax_backend_env`), `CLAUDE_CONFIG_DIR` for the Windows
sandbox — and a prefix rule would silently undo all three. Second, the scrub
runs *before* the harness environment policy, so a deliberately-set value is
layered back on afterwards by `agent::spawn::command`; deliberate wins by
ordering, not by having to be excluded from the list.

`scripts/run-dev.ps1` and `scripts/run-dev.sh` clear the same names for the dev
launch only, so agent-driven runtime checks behave like a user's own launch.
That is defence in depth for the paths the in-app scrub cannot reach, not a
substitute for it: the scrub is what fixes a *shipped* app a user launched from
a Claude Code terminal, which no script can help with.

Read `CLAUDE_SESSION_MARKER_ENV_VARS` before adding a variable: a new marker
Claude Code exports belongs there, and its own test asserts none of the
deliberately-set names above ever drift in.

## Harness hook config documents

Every adapter that injects a Buildmesh entry into a harness config does the same
three things: read the user's document, merge Buildmesh's own entry into it, and
persist the result without losing the user's content.
`src-tauri/src/agent/managed_config.rs` owns the read and the write for every
adapter that uses it, plus the merge wherever the document's shape fits one of
the two shared upserts. How much each adapter shares follows its document.

Above those primitives, `src-tauri/src/agent/attention_hooks.rs` owns the
attention-hook orchestration once (issue #2525): read, build the expected
entries, compare, upsert, write atomically and report whether anything changed.
AGY, Claude Code, Cursor and Grok declare their JSON shape — file, root key,
event list, matcher rules, handler shape and ownership anchors — as a
`HookProvisioningSpec`; Kimi declares the TOML shape through the same
`provision_attention_hooks` entry point, so a caller does not know which format
its harness writes. The module also owns the idempotency comparison and the
unreadable-file wording, so a policy fix lands once instead of once per copy.

Claude Code, Cursor, Grok and mcode share all three: their entry is a member of
an array of handlers or matcher groups, which is what `upsert_entry` and
`upsert_matcher_group` are for. AGY, Muse and the workspace-trust stores share
the read and the write but merge in their own way, because their entry is not a
member of a handler array — AGY owns one whole namespace key, Muse sets a
decision value keyed by workspace, and a trust store appends a path to a list of
values. Cline and OpenCode share only the writer: a hook script and a pure
plugin template have no document to parse and no user entries to preserve. Kimi
shares the refusal and the writer, because its config is TOML and its
`config.yaml` permission pin is a surgical single-line edit rather than a
round-trip. Codex shares only the writer, and only on its native arm.

**What the shared module owns.**

- `atomic_write` is the single temp-file-then-rename writer for these config
  documents. The temp file is a sibling of the target (the rename must stay on
  one volume) named with the process id and a per-process counter, so two
  concurrent provisions — or two `rust-test-shards` binaries writing the same
  directory — can never land on one name. On unix it is created `0o600` and the
  rename carries that onto the target: `File::create` would inherit
  `0o666 & !umask`, which would have widened `.claude/settings.local.json` inside
  the user's own repository among others.
- `read_document` treats a missing file as an empty document and refuses a
  malformed one through `refuse_malformed`, **one** wording for every harness and
  every format. Before issue #2235 that answer had drifted: AGY reported
  `invalid agy hooks.json …` where Cursor and Grok reported
  `refusing to overwrite malformed …`, on the same corruption, in the same user
  session. A file that cannot be read at all is a different failure and keeps
  its own wording — an adapter whose tests pin it (AGY's non-UTF-8 and
  directory cases) branches on `DocumentError::is_malformed`.
- `Document` carries whether the user's file ended in a newline, so a rewrite
  restores it instead of turning a hand-edited file into an unrelated diff.
- `carries_anchors` is the single ownership test: an entry is Buildmesh's when
  its stored command carries every marker the adapter declared. It decodes a
  `powershell.exe -EncodedCommand` line first, because the WindowsInterop relay
  base64-encodes the callback and an undecoded match would make a re-provision
  append a duplicate of our own hook instead of replacing it. For Claude Code
  and Grok that decode widened their predicates, which did not decode before
  #2235: a WindowsInterop-installed config is now recognised as ours and updated
  in place rather than duplicated beside a stale copy. Cursor and mcode already
  decoded, so theirs is unchanged.
- `upsert_entry` and `upsert_matcher_group` are the two merge shapes. Both
  report whether anything changed, which is what lets a re-provision skip the
  write entirely (issue #886's no-spurious-mtime invariant). Neither appends into
  a matcher group the user already filtered on — that would silently scope our
  hook to the tools they filtered.
- `root_object` / `object_entry` / `array_entry` reject a valid-JSON value of
  the wrong shape and name it (`got Array`), so a rejection points at the key
  the user has to repair.

**What stays with the adapter**, and must not be generalised away: which file,
which events, which handler shape, and which markers make an entry ours. Those
markers are what identify an already-installed config as Buildmesh's, so
changing one would stop Buildmesh recognising its own prior installs and append
a second copy instead of updating it.

Two shapes stay outside the module because only one harness has them. Codex
writes through the guest CLI on WSL (`read_runtime_files` / `write_runtime_files`,
a `wsl.exe` round-trip rather than a local rename) and *retires* Buildmesh's
entries by stripping them — a removal the merge does not model; its native arm
still uses `atomic_write`. Codex's `materialize_native_profile_at` is also a
separate temp-and-persist writer: it writes the Codex *profile* home rather than
a hook document, sets its own `0o600` on unix, and cleans up legacy profiles
afterwards.

## Saved Spawn Configurations

Companion's AI Assistant selection lives in Provider defaults and uses the
same `backgroundInferenceOption` UI decoration and `agent::background::resolve_plan`
backend validation as auto-naming. The selection is stored in
`companion.launch_configuration`, falling back to the app default and then
Claude Code. Its runner honors adapter prompt transport and final-answer
decoding on every inference step, with a distinct result file per step.
Assistant replies retain their 256 KiB bound; naming and classification retain
the shared decoder's 4 KiB default. Companion settings writes apply supplied
fields atomically, so changing its inference configuration and spawn limit
from separate settings panes preserves both edits.
See [ADR 0045](../adr/0045-companion-cli-tool-boundary.md) for fleet tool ownership.

Model-reference syntax belongs to the harness adapter through
`AgentProvider::validate_model_override`. Configuration and harness-default writes
invoke it after normalization; interactive command composition and background
launch resolution check the final model again. This catches Circuit overrides
and stored snapshots that bypass save validation. MiniMax Code restricts
`provider/model[#variant]` parts to ASCII letters, digits, `.`, `_` and `-` so
Windows Cmd cannot split or expand the reference. Syntax validation does not
prove account availability or CLI-version compatibility.

The desktop configuration submenu is a side flyout of the harness row it was
opened from. It opens to the right of that row, keeps the row's top line, and
scrolls inside its own cap instead of growing past the viewport; it opens
above the row only when there is no usable room below it. The row is the
anchor, so an anchor that has lost its layout keeps the last placement rather
than being re-measured at the viewport origin: a replaced row leaves the
submenu where it was instead of stranding it in the corner of the screen. The
anchor's edges are clipped to the viewport, so a row that is only partly
visible at a scroll boundary no longer hands the submenu room past the
viewport edge. The placement follows the panel's content size, because the
editor owns its own draft state and can add or remove fields — a routed
provider adds endpoint fields — without changing any of the submenu's props.

The shared editor discloses its advanced inputs instead of laying the fields out
in one continuous form. The raw values — the extra arguments passed to the
harness command line, and the custom model reference a `Custom model…`
selection needs — live behind one `Advanced options` disclosure; the
endpoint, the model choices and the verification action stay visible outside
it, because a route that needs them still needs them filled in. The
disclosure opens by itself when the edited configuration already carries an
advanced value, and it stays open while the custom model input is required,
so a required field does not sit behind a closed section. The disclosure is a
frontend presentation concern: it changes no stored configuration, no
capability rule and no wire type.

`preferences::spawn_configurations` owns named, capability-validated launch overrides scoped to one Spawn Option. Configurations live in application preferences; the backend menu includes each option's saved choices for mobile, while desktop management reads the same collection through IPC. The shared editor creates and edits configurations from Settings and spawn menus. Launch targets include unattached credentialed providers; saving a new route and recipe uses one preference transaction. Draft verification resolves the selected model without persisting the draft; verification records distinguish endpoint/model/runtime so checking one recipe does not replace another model's proof. Provider model metadata is independent of tier remaps, and allowed efforts intersect provider/model/surface metadata with harness capabilities. New-node creation commits the selected snapshot in `agent_nodes.spawn_configuration` in the same transaction as the node. Explicit per-call overrides win; omitted native fields retain the mesh/application/native cascade, while proxy models default to their route and do not inherit native harness model/effort defaults. `preferences::harness_default_for` is where that exclusion lives — it takes the parsed `SpawnOptionId`, reads the harness half's map entry (native and proxied rows share one key), and strips model/effort for a proxied row — so the frozen-configuration resolver and the live `agent::spawn::launch` seam cannot drift apart. Permission mode stays: it is a harness-CLI flag, not an endpoint fact, so a proxied spawn still inherits it. A resolved proxy model reaches Codex as a single `--model`: `agent::spawn::command::build_spawn_command_prepared` folds the routing descriptor's model into the resolved config before `default_prepare` composes the recipe, so the adapter remains the single owner of the model flag and the orchestrator layer adds only `--profile` and the reasoning `-c` keys. The fold is load-bearing in both directions: the generated `<profile>.config.toml` carries only `model_provider`, so an empty cascade model would leave Codex on an OpenAI model against a foreign endpoint, and a second occurrence is rejected by the CLI as a repeated argument. Resume reads the snapshot, not the editable preference. A provider change cannot reuse another Spawn Option's snapshot.


For scoped project launch directories, use `env::node_working_path`; checkout
ownership and Git watcher identity use separate helpers. See
[Agent Node worktree support](agent-nodes.md#worktree-support-git2-based).

## The Spawn Menu derivation is memoized per preferences generation

Both production readers of the Spawn Menu — `available_providers` (the mobile
route `GET /api/providers`) and `list_providers_blocking` (the `list_providers`
Tauri command) — go through `agent::provider_menu::derived_menu`, which serves a
memoized `Vec<ProviderInfo>` instead of re-deriving.

Clients read the menu on a timer, not on demand: the Android client polls its
mesh/node/provider snapshot every 5s while foregrounded (`BuildmeshViewModel`), so
a paired phone idling on the node list used to re-derive the whole menu — the
per-runtime Codex probe included, and a failing probe is discarded rather than
served warm (`CodexInstallCache::discard_failed_entry`), which is why the
warm-native / cold-foreign split put 400-700ms in every pass — twelve times a
minute, each pass writing a log line. A cached read is what removes that rate; a
quieter log level alone would not, because the default filter already admits this
crate at `debug`.

The memo is keyed on `preferences::generation`, which bumps on every durable
preference write (`save` / `update`), and a change reaches the rest of the app only
once one of those lands — so a memoized menu is never older than the preferences
cache every other reader sees. Generation handling matches
`agent::launch_routing`: it only moves forward, so a derivation that sampled an
older generation can neither publish nor evict a newer entry. What the generation
cannot see is a change made outside Buildmesh (a `npm i -g @openai/codex`, or a
WSL-side install), so the entry also carries a 45s TTL — the same bound
`codex::CODEX_INSTALL_CACHE_TTL` already accepts for install metadata.

The derivation's single log line is `debug`, not `info`: it carries the same
wall-clock and Codex-probe attribution fields it always has, for reading next to a
slow save-refresh, without ranking a per-poll line above the attention and
lifecycle lines it used to bury. Its test drives the derivation directly, because
the memo turns a second `available_providers()` call in one generation into a hit.
