# 37. Usage Meter last-known fallback (durable, 7-day TTL)

Status: accepted

A Usage Meter whose provider cannot be reached with a usable credential falls
back to the last reading that provider reported, if one was recorded within the
last seven days. The value is labelled as last known. With nothing recorded, the
meter is hidden exactly as it was before.

## Context

Several first-class meters only report usage while their harness credential is
fresh. Antigravity (`agy`), Grok, and Meta Muse each read a local OAuth/login
artifact that the harness refreshes only when the user signs in there. So the
common start-of-day case is a fresh Buildmesh launch where none of those
harnesses have been signed into yet: every fetch returns no usable credential and
their meters disappear from the Usage tab.

Two existing mechanisms do not cover this:

- `services/usage/cache.rs` is a **five-minute in-process** TTL cache keyed by
  `UsageIdentityFingerprint`. It keeps a reading *fresh* within one process and
  dies with that process, so it cannot help after a restart — and its identity
  digest is process-salted (`rand::random()`), so it cannot key anything on disk.
- The keep/drop gate in `commands/usage.rs::assemble_meters` is keyed on
  `UsageOutcome::keep()`. `NoCredential` always drops the row, and `Rejected`
  drops for native providers (only non-native adapters appear in
  `configured_keyed_provider_ids`). Those are exactly the cases this decision
  addresses.

## Decision

1. **The fallback applies only where the row is hidden.** It replaces the row
   *only* when `UsageOutcome::keep()` says drop — i.e. when the meter would be
   hidden today. Kept outcomes are untouched:
   - `RateLimited` / `Unavailable` keep showing their live error, because that is
     a real signal the user may need to act on.
   - `Rejected` with a configured key keeps the "Invalid API key" prompt — the
     user's route back to Settings. For a *native* provider with no configured
     key, `Rejected` is itself a drop and therefore does fall back; that is the
     expired-OAuth-token case that motivates the change.
   - A live `Reading` is never replaced (issue #1073).
2. **Durable, keyed by provider id, seven-day TTL.**
   `services/usage/last_known.rs` persists `usage_last_known.json` in the app
   data dir. The TTL is measured from the last successful fetch, so a provider
   polled daily never expires.
3. **The row is stamped and labelled.** `ProviderMeters.cachedAt` carries the
   epoch seconds the reading was fetched, and the Usage tab renders
   `Last known value · <relative>` so a stale figure is never mistaken for a
   live one.
4. **Best-effort, never a gate.** A missing, unreadable, or corrupt cache file
   reads as empty and can never fail or alter a live probe.

## Alternatives considered

- **Key by `UsageIdentityFingerprint` (so a cached reading follows the
  credential).** Rejected: the fingerprint is salted with a per-process random
  value, so a persisted key would never match after a restart; and
  `MuseCodeAdapter::cache_identity` hashes the access token itself, so the token
  rotation that *causes* the fallback would also be a guaranteed cache miss.
  Provider id is also the granularity at which the Usage tab renders one row.
- **Extend the five-minute in-process cache to a longer TTL.** Rejected: it still
  dies with the process, which is the exact case — start Buildmesh for the day,
  nothing fetched yet — the fallback exists for.
- **Fall back on every non-`Reading` outcome.** Rejected: it would replace live
  rate-limit and transport errors with stale numbers and hide the "Invalid API
  key" affordance, discarding signals the UI deliberately surfaces.
- **Store the reading inside `preferences.json`.** Rejected: this is a disposable
  cache, not a preference. It should not share the preferences write path, its
  in-process cache, or its migration surface.

## Consequences

- Antigravity, Grok, and Muse Code meters stay visible across restarts on days
  when their harness has not been signed into yet — up to seven days from the
  last successful fetch — and are always labelled as last known.
- A user who removes a provider's credential but leaves the account enabled can
  still see that provider's last reading until the TTL expires or a fetch
  succeeds. Disabling or removing the account removes the row as before.
- `ProviderMeters.cachedAt` is additive (`#[serde(default)]` plus
  `#[ts(optional)]`), so older fixtures and frontends remain valid.
- The cache holds no secrets: usage percentages, balances, and the provider id —
  never a token or key. It is not invalidated on credential change; the
  coarse provider-id key is deliberate (see Alternatives).

## Verification

- `src-tauri/src/services/usage/last_known.rs` tests: record/snapshot round trip,
  7-day expiry, refresh of the timestamp, the two record guards, corrupt-file
  tolerance, no-directory mode, and a cross-instance round trip modelling two app
  runs over the same directory.
- `src-tauri/src/commands/usage.rs` tests: fallback fills a dropped row and stamps
  `cached_at`; nothing remembered still drops; `Unavailable`, a rejected
  configured key, and a live `Reading` are never replaced.
- `tests/unit/usage-panel.test.tsx` and `tests/unit/usage-tab.test.tsx`: the label
  renders with a relative age and is absent for a live row.

## Documentation

- [Knowledge primer](../knowledge-primer.md) — "Usage Meter last-known fallback".
- [v1.3.0 release notes](../releases/v1.3.0.md).
