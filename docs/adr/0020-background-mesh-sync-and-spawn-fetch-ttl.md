# ADR 0020 — Background mesh sync, spawn-time fetch TTL, and warm pool on by default

## Status

Accepted (2026-07-11)

## Context

Buildmesh's core interaction — click Spawn, get a working agent terminal — has
accumulated latency from three individually-reasonable decisions:

1. **ADR 0001** put a synchronous `git fetch` + `git pull --ff-only` on every
   fresh-worktree spawn so a new Agent Node always starts from the latest
   upstream. That's a network round-trip (typically 0.5–3s, bounded at 30s by
   `SPAWN_FETCH_TIMEOUT`) sitting directly on the click-to-terminal path, paid
   again on every spawn even when the same Mesh was fetched seconds earlier.
2. **The pre-spawn worktree pool** (PRD #608, issues #609–#653) eliminates the
   multi-second cold checkout — but shipped **off by default**
   (`pre_spawn_pool_size DEFAULT 0`), so in practice most spawns stayed cold.
3. **Nothing fetched in the background.** The pool worker refilled worktrees
   but never fetched, so pool freshness depended entirely on spawn-time
   fetches — and manual warm-claim spawns *skip* the fetch (the pool path
   already exists), so a manual-spawn-only workflow could drift stale
   indefinitely.

The correctness goal behind ADR 0001 — "a new worktree starts from the latest
upstream" — does not actually require the fetch to happen *at spawn time*. It
requires the Mesh to be *recently fetched*.

## Decision

Three coordinated changes:

### 1. Background mesh sync (`services::pool_worker`)

The existing idle-gated pool worker now also runs the spawn-style auto-sync
(`git::sync::locked_fetch_origin_blocking` — same per-Mesh sync lock, same
dirty-skip / no-remote-skip / `--ff-only --no-rebase` semantics, same 30s
timeout) for each worktree-enabled Mesh, at most once per
`BACKGROUND_SYNC_INTERVAL` (3 min) per Mesh. When the fetch advances the base
ref it triggers the existing `warm_pool::on_fetch_completed` freshness pass,
so pool entries are continuously re-pointed at the latest SHA — closing the
manual-warm-claim staleness gap for free.

The cadence is gated on the last *attempt* (not last success), so an offline
machine retries once per interval, not once per 2s tick.

### 2. Spawn-time fetch TTL (`services::fetch_freshness`)

An in-memory per-Mesh registry records when a fetch last *succeeded* (stamped
by the background sync, spawn-time fetches, and the manual Sync command — all
of which funnel through the two `locked_*` sync wrappers). The spawn path
skips its blocking `fetch_origin` when the Mesh's last successful sync is
younger than `SPAWN_FETCH_TTL` (5 min).

- The registry is in-memory only: fetch recency is meaningless across an app
  restart, so the first spawn/worker pass after launch always re-syncs.
- The **PR-head fetch is never skipped** — landing on the PR's actual commits
  is correctness, not freshness, and the SHA-drift check (#444) depends on it.
- **User override:** the manual Sync button fetches unconditionally (and
  re-stamps the registry, making the next spawn instant).

### 3. Warm pool on by default (schema v24)

`pre_spawn_pool_size` now defaults to **1** for new meshes, and a one-time
backfill flips existing worktree-enabled meshes still at 0 to 1. The backfill
is gated on its own `app_settings` flag (`pool_default_backfill_v24`), written
only after the UPDATE commits — crash-safe, and a user who later opts a mesh
back to 0 is never overridden again. Worktree-disabled meshes are untouched.

Trade-off acknowledged: a pre-v24 explicit 0 is indistinguishable from
never-configured (the same COALESCE-default ambiguity as `base_ref`), so the
flip overrides both — once. The Worktrees Probe remains the opt-out.

## Consequences

- **Steady state, warm pool hit:** click-to-terminal drops to roughly the PTY
  + agent-CLI boot time. No network round-trip (TTL skip), no checkout
  (pool adopt), for manual AND issue/PR spawns.
- **Worst-case staleness** of a new worktree is bounded at ~`SPAWN_FETCH_TTL`
  (5 min) of upstream commits, and in practice ~3 min (the background
  cadence). ADR 0001's warning toasts are unchanged for the cases that still
  fetch.
- **Disk/IO cost:** one standing pre-warmed worktree per mesh, plus a
  narrow single-branch `git fetch` per mesh every 3 minutes while idle. Both
  are bounded and idle-gated; the fetch reuses every existing serialization
  primitive (per-Mesh sync lock) so it cannot collide with spawns or manual
  syncs.
- The `spawn_timing:` log gains a `fetch_origin_skipped_fresh` checkpoint and
  the spawn diag stream a `fetch_origin skip:fresh` event, so the skip is
  observable and regression-guardable.

## Relationship to prior ADRs

- **ADR 0001** (auto-sync on spawn): semantics preserved, timing relaxed from
  "at spawn" to "within the TTL". The sync algorithm, dirty-skip policy, and
  warning surfaces are untouched.
- **ADR 0004** (optimistic close): unchanged — close was already off the hot
  path; this ADR is the spawn-side counterpart.
- **PRD #608 / issues #609–#653** (warm pool): the pool's "PR-style fetch
  intentionally omitted" note in `services::warm_pool` is now resolved by the
  background sync + `on_fetch_completed` pairing.
