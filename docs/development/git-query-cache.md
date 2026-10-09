# Git query cache: freshness and retention

Status: current

The frontend's Git reads (`git status`, `git summary`, branch status, mesh
health, open PR, changed files) run through one primitive,
`src/lib/pathInvalidatedCache.ts`. Every hook client is a module-level
singleton built by `createPathKeyedCache` (key is a repo path) or
`createDualKeyCache` (key is an entity id such as `nodeId`/`meshId`), and
React glue in `src/hooks/usePathInvalidatedQuery.ts` reads through
`client.read` / `client.refresh`.

That shared singleton is process-lifetime state. This document is the
contract for how much of it buildmesh keeps, and for how fresh a value read
out of it is allowed to be.

## Three independent clocks

A key's freshness is not one setting. Three distinct mechanisms, often with
very different values, decide what a caller sees:

| Mechanism | Option | Question it answers |
|---|---|---|
| Refetch-rate window | `minRefetchIntervalMs` | While a key is **watched**, how often may a `GIT_CHANGED` burst trigger a full-repo walk? Bounds the steady-state load while an agent streams edits. |
| Inactive-entry TTL | `inactiveEntryTtlMs` (default 5 min) | A key with **no subscriber** — how long may its cached value still be served on a revisit? |
| Inactive-entry cap | `maxInactiveEntries` (default 200) | How many keys with no subscriber may this client retain at all? |

The rate window is *not* a freshness guarantee. It bounds how often an
invalidating event may be honoured; the value it serves afterwards is a
freshness question, which is the TTL's job.

## Active keys are pinned

A key is **active** from its first keyed `subscribe` until its last one
unsubscribes. While active it is exempt from both the TTL and the cap. A
mounted panel's value can never be pulled out from under it by the passage
of time or by churn on other keys, and its `minRefetchIntervalMs` window
always applies on top.

Pinning is reference-counted, so one key with several subscribers stays
active while any of them remains. The unsubscribe closure releases the count
exactly once and is safe to call twice.

## The revisit contract

`GIT_CHANGED` is dispatched only to **mounted** subscribers. A key nobody is
watching therefore cannot be invalidated by an edit that lands while it is
off screen — its cached value silently goes stale, and nothing would correct
it until some unrelated event happened to match its path.

`read` is the freshness gate that closes this, and it is lazy: an inactive
entry is retired **when it is read**, not by a sweep timer.

- Revisit **inside** `inactiveEntryTtlMs` → the cached value is served, with
  no refetch. This is what keeps closing and reopening the Probe, or
  switching to another node and back, a cache hit rather than a git walk.
- Revisit **past** the TTL → the entry is retired and `read` reports
  *uncached*, so the caller refetches.

The consequence to keep in mind: a key off screen is only guaranteed to be
at most `inactiveEntryTtlMs` stale, not fresh. Set the TTL to the staleness
you are willing to display.

A key with a request still in flight is never TTL-retired by `read`. The
hook reads the cache *before* it subscribes, so retiring a running fetch
there would cancel a request the returning subscriber was about to adopt;
the completion re-stamps the key when it settles.

## Bounded cardinality

`inactiveSince` is an insertion-ordered `Map` from key to the timestamp at
which its last subscriber left. It is simultaneously:

- the **LRU** — Map iteration order is eviction order, so over-cap eviction
  takes the least-recently-inactive key with no extra index;
- the **TTL store** — the stamp is the value in the same entry;
- the **occupancy list** — a key is present only if it holds something.

A key occupies a slot when it holds a cached result (including a cached
`null`), a recorded error, **or an in-flight request** — a promise chain is
retained memory too. Including the pending case is load-bearing: it is what
makes the late-completion fence below reachable, because a key that is
unsubscribed but not yet settled is exactly the shape that could otherwise
resurrect itself.

`refresh` is reachable without a subscriber (imperative callers such as
`refreshOpenPrForNode`), so a settle that finds no live subscriber records
the key as inactive at that moment. The cap applies to those keys too.

`invalidate` drops the entry's stamp, so an emptied key does not sit in the
LRU counting against the cap.

### Late-completion fence

Eviction deletes the key's `pending` slot. Every completion already in flight
checks `pending.get(key) !== p` before it writes, so a completion that lands
after eviction adopts whatever is current (or nothing) and **cannot
resurrect the entry**. The pending slot's identity is the whole fence — there
is deliberately no generation counter and no tombstone map, because either
would grow unbounded for exactly the deleted-entity keys this policy exists
to reclaim.

## `null` versus uncached

`read` returns `undefined` for *uncached* and `null` for *cached, and known
to be empty*. Callers depend on that distinction: "no open PR" and "we have
not asked yet" are different states. Expiry always retires to `undefined`,
never to `null`, so an expired empty answer is refetched rather than
mistaken for a current one. `lastError` is gated on the same clock — a
recorded failure is per-key retained state and can never outlive the value
it belongs to.

## Deleted entities

Nothing routes a deletion event into the cache, and it does not need to.
Mesh and Agent Node deletion is reclaimed by the same policy as any other
inactive key: an id that never comes back ages out on the TTL and is evicted
by the cap on its own. Keys pinned by a live subscriber are released when
that subscriber unmounts, which is what the deletion flows already do.

## Per-entity promise maps

Two other per-entity caches sit outside the primitive and are **not** covered
by the cap, so they are evicted at the lifecycle boundary instead:

- `src/lib/providerCache.ts` — `defaultProviderByMesh`, keyed by Mesh id.
  Evicted on rejection, on a `default_provider` column write, on an app-wide
  default change, and on Mesh deletion.
- `src/lib/tauri.ts` — `scratchpadByMesh` / `scratchpadWritesByMesh`, keyed by
  Mesh id. Evicted on rejection and on Mesh deletion.

Each eviction only drops the slot if it still holds *that* promise, so a
concurrent re-populated entry survives a stale cleanup.

## Tests

`tests/unit/path-cache-retention.test.ts` covers the retention contract:
settled cardinality after thousands of query/switch/delete cycles, active
pinning under churn and under time, both sides of the revisit window,
late-completion and late-rejection fencing, `null`/uncached semantics, error
expiry and isolation, and the dual-key shape.

`tests/unit/path-cache-request-ownership.test.ts` covers the neighbouring
concern — request coalescing, supersession, and trailing-refetch cleanup —
which is a separate axis from how much the cache keeps.