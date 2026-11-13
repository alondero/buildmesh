# OpenCode Go usage investigation

Status: Investigation checked 2026-11-08 against Buildmesh base
`5fbe38826efd5c4b7f278e94c29738e9d16d754a`. For maintainers implementing the
Usage Meter; the current contract belongs in
[providers](../development/providers.md).

## Finding

Use `GET https://opencode.ai/inference/go/v1/usage` with a Go API key in
`Authorization: Bearer <key>`. OpenCode's
[October 21 endpoint clarification](https://github.com/anomalyco/opencode/issues/52227)
identifies this canonical URL and says `/zen/go/v1/usage` remains supported.
OpenCode merged the original endpoint on 2026-08-11 in
[PR 16513](https://github.com/anomalyco/opencode/pull/16513), explicitly to expose
the dashboard's usage. A subsequent
[same-day response simplification](https://github.com/anomalyco/opencode/blob/d4704347465c1ee63d0c213ed00e648e7f0231c5/packages/console/app/src/routes/zen/go/v1/usage.ts)
established this mapping:

| API field | Buildmesh window | Used percentage | Reset instant |
|---|---|---|---|
| `usage.rolling` | 5-hour | `percent` | `resetsAt` |
| `usage.weekly` | Weekly | `percent` | `resetsAt` |
| `usage.monthly` | Monthly | `percent` | `resetsAt` |

Each window also carries `status`, historically `ok` or `rate-limited`. A limit
being reached is still a successful meter reading. The route returns JSON and
ISO UTC reset timestamps; it supplies neither a wallet amount nor a dollar
limit. The published implementation uses 401 for absent/rejected API keys and
403 `EntitlementError` when the key's user has no Go subscription. Neither a
workspace ID nor console OAuth is needed when using a Go API key. These statements are
from the response implementation linked above, rather than an invented fixture.

## Native OpenCode 2 credentials

The current
[OpenCode 2 usage reader](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/core/src/usage-limits.ts)
confirms the same three-window schema. It resolves the active `opencode`
integration, accepts either OAuth `access` or a key, and sends the selected
workspace as `x-opencode-org-id`. It still calls `/zen/go/v1/usage`. Thus OAuth
is a supported route when supplied with the Console workspace scope; the old
Buildmesh `billing.get` RPC is not required.

The native
[credential table](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/core/src/credential/sql.ts)
stores `value` JSON and `integration_id`, with an `active` flag.
The [credential reader](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/core/src/credential.ts)
and [connection selector](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/core/src/integration.ts)
prefer explicit active credentials, then newest creation time and ID. Legacy
rows without an active flag use the newest row. OAuth is refreshed within five
minutes of expiry, under a per-credential cross-process lock with a re-read.

The [value schema](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/schema/src/credential.ts)
defines keys as `{type:"key",key,metadata?}` and OAuth as
`{type:"oauth",methodID,refresh,access,expires,metadata?}`. Expiry is Unix
milliseconds; `metadata.orgID` is the workspace. Older key rows may have
`configuration` answers that
[override matching metadata](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/schema/src/credential-legacy.ts).
The [Console plugin](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/core/src/plugin/provider/opencode.ts)
uses OAuth method `device` and client ID `opencode-cli`; refresh posts JSON to
`https://opencode.ai/console/auth/device/token`, preserving metadata and rotated
tokens. Shared native refresh requires native locking rather than overwriting
its token behind a running process.

The [native data roots](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/util/src/global-roots.ts)
use `XDG_DATA_HOME/opencode`, falling back to `~/.local/share/opencode` on
Windows as well as Unix. The
[CLI database selector](https://github.com/anomalyco/opencode/blob/caf8d6a554ecd988767c08f1f15508849eff8728/packages/cli/src/database-path.ts)
honors `OPENCODE_DB`, resolved relative to that data directory unless absolute.
Default production channels use `opencode.db`; other channels may use a
channel-specific filename. `OPENCODE_CONFIG_DIR` alone changes configuration,
not the credential database. Select the configured store, rather than merging
credentials from every channel.

## Reset semantics

Buildmesh should preserve the returned percentage and instant. The following
explains the historical server algorithm, not a client-side replacement for it.

The published
[subscription analyzer](https://github.com/anomalyco/opencode/blob/d4704347465c1ee63d0c213ed00e648e7f0231c5/packages/console/core/src/subscription.ts)
calculates used percentage as `floor(min(100, usage / limit * 100))`, returning
100 with `rate-limited` at the limit. Expired usage contributes zero. Rolling
reset is the server's recorded rolling anchor plus the configured window; an
expired rolling anchor produces zero usage and a full window from now. It is
not calculated from the creation time of local sessions. The route formats its
remaining seconds as an absolute ISO timestamp.

The published
[date utility](https://github.com/anomalyco/opencode/blob/d4704347465c1ee63d0c213ed00e648e7f0231c5/packages/console/core/src/util/date.ts)
defines the weekly period as Monday 00:00 UTC through the next Monday 00:00 UTC.
Monthly periods preserve the subscription's UTC day and time. Shorter months
clamp the day to their last day; the next month uses the original subscription
day again. Thus monthly is neither a trailing 30-day sum nor necessarily the
first day of a calendar month. Preserve fractional seconds and timezone when
passing `resetsAt` into Buildmesh's existing reset display.

## Current evidence boundary

The
[October 12 route](https://github.com/anomalyco/opencode/blob/b9ca4d21b4f241154081143a6f5bef648f5ccfd0/packages/console/app/src/routes/zen/go/v1/usage.ts)
delegates to an inference proxy. Its
[published proxy mapping](https://github.com/anomalyco/opencode/blob/83f2b6f67a5245214f4bb365ff55f7dcba9ce7fa/packages/console/app/src/lib/inference-proxy.ts)
forwards this exact GET route to `/go/v1/usage` and preserves bearer
authentication. The legacy Console sources were subsequently removed from
`dev`; historical algorithms are explanatory evidence, not a guarantee about
today's private server internals. Current public native client code and the
authenticated probe below confirm the exposed API shape.

The investigation's read-only authenticated probe used the native active
Console credential, its workspace header, and the canonical inference URL. It
returned HTTP 200 with:

| Window | API used percentage | API reset UTC |
|---|---|---|
| 5-hour | 0 | `2026-11-08T15:29:19.000Z` |
| Weekly | 0 | `2026-11-09T00:00:00.000Z` |
| Monthly | 0 | `2026-12-08T10:21:22.000Z` |

All three statuses were `ok`. A read of the signed-in Console's
`/console/api/go/status` with `x-org-id` reported the same reset instants and
`11861046` used microcents against five-hour/weekly/monthly allowances of
`1200000000`, `3000000000`, and `6000000000`. The dashboard displayed 1% for
five-hour while the usage API reported 0%, consistent with rounded UI and
floored API percentages below 1%. Preserve the API number instead of attempting
to reproduce dashboard rounding. These are point-in-time account observations,
not fixtures that guarantee other plans' dollar limits.

The local legacy `auth.json` Go key returned HTTP 401 `api_key_expired` from the
canonical endpoint. It must not override a usable active Console credential or
turn a rejected key into a zero quota reading. No credential, account ID,
workspace ID, or payment data is retained in this note.

The current [Go documentation](https://opencode.ai/docs/console/go/) describes
three plan windows, with five-hour and weekly allowances of 20% and 50% of the
monthly allowance. Model allowances and Go/Go Plus/Go Teams plans vary, so a
client should not infer quota percentages from a fixed dollar allowance.

## Baseline discrepancies and implementation direction

At the recorded Buildmesh base,
[the OpenCode adapter](../../src-tauri/src/services/usage/adapters/opencode.rs)
parses an unverified `windows` array from a console `billing.get` RPC. Its
fallback sums entire sessions by creation time across providers, divides by
fixed $12/$30/$60 limits, and emits no reset times. Missing databases even
produce zero-percent windows. Those values cannot establish account quota.

Replace that meter path with the key-authenticated usage endpoint and strict
validation of its three server-reported windows. Preserve shared unavailable,
authentication, caching, and last-known behavior instead of inventing fresh
percentages after a failed read. Keep local session telemetry out of account
quota. Resolve the active native credential and preserve its workspace scope;
never hide its rejection by switching to an unrelated stale key or reporting
local cost as current quota.
