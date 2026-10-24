---
name: perch-harness-capabilities
description: Capability contract and subscription usage meter for the Perch CLI harness
metadata:
  type: reference
  harness: perch
  verified_cli_version: 2.4.116
---

# Perch harness and subscription usage

Audience: maintainers changing the Perch adapter or its usage meter.
Status: current against perchai-cli 2.4.116 on 2026-10-24.

Perch is both an interactive coding CLI and a subscription. Buildmesh launches
the interactive `perch` process, resumes it with the thread id it minted, and
reads that account's hosted allowance in Perch Tokens (PT). Install and sign-in
stay with the Perch CLI. Buildmesh does not call `perch login`, and a failed
token refresh does not delete the saved session.

## Sources

Verified against:

- Installed `perchai-cli` 2.4.116 (`dist/perch.mjs`). `perch --version` and
  `perch --help` are unknown flags; the version is the package's `package.json`.
- Public docs: [Using the CLI](https://www.perchai.app/docs/using-perch/cli),
  [Starter plan](https://www.perchai.app/docs/plans/starter),
  [Pro plan](https://www.perchai.app/docs/plans/pro), and
  [pricing](https://www.perchai.app/pricing).
- Buildmesh adapters: `src-tauri/src/agent/provider/adapters/perch.rs` and
  `src-tauri/src/services/usage/adapters/perch.rs`.

The account response shape comes from that CLI source. A live
`GET /api/perchai/account` with an expired access token returned HTTP 401
`not_authenticated`. No live account snapshot was captured, so the meter tests
use synthetic fixtures.

## Launch

`perch` opens a terminal UI and does not take a positional prompt. `perch run`
exits after one request, so it is not the durable node. Prefill stays off.
There is no one-shot background recipe.

| Platform | Spawn |
|---|---|
| Windows | `perch` through `cmd.exe`, because the npm install is a `.cmd` shim |
| macOS and Linux | `perch` directly |

The binary is advertised on Windows, macOS, and Linux. Detection looks for
`perch` on `PATH` and a `.perch` config directory. Perch is not excluded from
WSL cross-runtime probing.

A fresh unattended launch is `--permission take_the_wheel --thread <id>`.
Prompt mode is `--thread <id>` with no permission flag, so the CLI keeps the
saved human session. The default in Buildmesh is unattended. Model and effort,
when set, follow as `--model` plus one argument and `--effort` plus one level.
A model value such as `standard 1m` is a single argument. Empty values, a
leading `-`, and CR or LF are rejected.

The closed effort set is `off`, `low`, `medium`, `high`, `xhigh`, `max`, and
`ultra`. The CLI also has `--thinking`. Buildmesh does not pass it.

Buildmesh mints the thread id and passes `--thread` on both a fresh spawn and
a resume. Auto-resume on startup is on. Perch does not print the id for later
capture. Thread files live under `~/.perch/threads/` (overridable with
`PERCH_CLI_STATE_DIR`). Buildmesh does not poll them.

There is no attention hook, no passive turn watcher, and no transcript reader.
Extra CLI arguments from a saved configuration are forwarded. A Perch node can
resume its process, and it does not appear in the archived-node picker.

## Published allowances

Public plan docs, checked 2026-10-24. One PT is $0.001 of hosted-model usage.

| Plan | Allowance |
|---|---|
| Starter | 20,000 PT each billing period |
| Pro Roost | No fixed monthly cap. 5,000 PT per rolling 5 hours and 20,000 PT per rolling 7 days |
| Pro manual selection | 15,000 PT per rolling 5 hours, 37,500 PT per rolling 7 days, and 75,000 PT per billing period |

Bring-your-own-key usage is billed by the user's provider and does not consume
the hosted allowance.

## Usage meter

The provider account id is `perch`, the same id as the harness, and the CLI
owns the credential. The meter's link is the Pro plan guide, labelled
**Usage guide**. The Starter guide is the matching page for the free plan.
Neither URL is a settings deep link.

The session file is `~/.perch/cli-auth-session.json`, or
`$PERCH_CLI_AUTH_DIR/cli-auth-session.json`. On macOS the session itself is the
login keychain item `app.perchai.cli-auth` / `default`, unless
`PERCH_CLI_AUTH_DIR` is set. The refresh lock and the invalid-grant marker
always stay in that directory.

A usable session has version 1, a non-empty `appUrl` and `accessToken`, and an
`updatedAt`. `expiresAt` is optional. A token is treated as expired at
`now + 90` seconds, including equality. The CLI's stored `appUrl` is the
origin for later requests. In 2.4.116 the CLI's own default web origin is
`https://chat.perchai.app`.

Buildmesh refreshes when the access token is inside that skew and a refresh
token is present, and once more when the account call returns 401. Refresh
takes the exclusive lock `cli-auth-refresh.lock` (a lock older than 30 seconds
is replaced; otherwise the call waits up to 15 seconds). It reads
`GET {appUrl}/api/perch-terminal/cli-auth/config`, then posts the refresh token
to that config's Supabase token endpoint. The rotated refresh token is written
back. An invalid grant records a one-day hash marker and leaves the session
file in place. A network failure, a bad config, or any other non-grant error
does not write that marker and does not delete the session.

The meter then calls `GET {appUrl}/api/perchai/account`. Row shapes follow the
CLI's own display rules:

- Monthly hosted spend is `ceil(usd / 0.001)` PT. Rolling windows are
  `round(usd * 1000)` PT and are omitted when the cap is not positive.
- A reset time is kept only when `nextFreedAt` parses. Buildmesh does not
  invent one. Starter and Pro monthly rows have no reset in the CLI.
- Starter (and a plan labelled Starter) is one Monthly row. The default limit
  is 20,000 PT. A positive `usage.monthly_pt` entitlement replaces it.
- Pro does not show a monthly Roost percentage. The detail notes "Roost fair
  use", then the enabled 5-hour and 7-day Roost windows, then Manual monthly
  (75,000 PT unless `usage.manual_monthly_pt` is a positive entitlement) and
  the enabled manual rolling windows.
- Admin, internal membership, or an `internal` plan is one Monthly meter with
  no individual limit, showing the PT already used. It is not a percentage bar.
- `billingLane` exactly `byo` is reported as managed by the user's own API key,
  with no quota rows.
- Top-level daily and weekly PT fields feed the CLI's activity line. They are
  not quota windows.
- Positive `creditBalancePt` is mentioned in the detail. It is not an ISO
  currency balance.

A missing session reports that Perch is not signed in and points at
`perch login`. An expired login that cannot be refreshed reports that the
login expired and points at the same command. Other failures say the account
was unavailable and do not echo the response body.

## Related

- [Harness capabilities matrix](harness-capabilities-matrix.md)
- [User guide: harnesses](../user-guide.md#harnesses-and-capabilities)
- [Provider usage contract](../development/providers.md)
- [Usage page destinations](../research/provider-usage-pages.md)
