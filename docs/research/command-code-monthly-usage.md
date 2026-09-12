# Command Code monthly usage meter

Investigated 2026-09-12. This note records the provider data contract used by the accompanying Monthly meter implementation. Initial research was read-only; implementation and its verification followed separately.

## Finding

Buildmesh can reproduce Command Code Studio's Monthly meter by enriching its existing credit request with the CLI's subscription request. The monthly percentage must use remaining **monthly** credits, not the combined monthly, purchased, and free balance.

The requested [account usage page](https://commandcode.ai/alondero/settings/usage) redirects unauthenticated requests to sign-in. Its deployed public JavaScript nevertheless establishes the actual calculation and data dependencies; no browser session or credentials were required to inspect those assets.

## Studio's calculation

The [monthly meter module](https://commandcode.ai/assets/monthly-usage-meter-Bq1uG83y.js) calculates:

1. Resolve the subscription `planId` to its tier.
2. Determine total monthly allowance: if `monthlyCreditsGranted` is positive and finite, take the maximum of that grant and the tier's base total. Otherwise use the tier total; organization tiers additionally multiply by valid, floored seat quantity (default one).
3. Clamp remaining monthly credits to at least zero. Clamp used credits (`total - remaining`) between zero and total.
4. Display the whole-number rounded percentage `used / total * 100`.
5. Show the meter only with a recognized tier, positive total, and subscription status other than `past_due`.

The [usage route module](https://commandcode.ai/assets/usage-Dbi36U2o.js) passes `subscription.currentPeriodEnd` to the reset display and feeds the calculation `credits.monthlyCredits`, optional `credits.monthlyCreditsGranted`, subscription `planId`, `status`, and `quantity`. Studio formats the reset date in UTC. Its calendar billing cycle is separate from the rolling five-hour and weekly windows, as confirmed by the [official limits documentation](https://commandcode.ai/docs/resources/pricing-limits).

The [deployed tier table](https://commandcode.ai/assets/plan-tiers-jQ-Ryb7R.js) matches the installed CLI 1.53.1 table inspected during this investigation:

| Plan ID | Base monthly credits |
| --- | ---: |
| `individual-go` | 10 |
| `individual-goat` | 70 |
| `individual-pro` (legacy) | 30 |
| `individual-pro-v1` | 80 |
| `individual-provider` | 15 |
| `individual-max` | 150 |
| `individual-ultra` | 300 |
| `teams-pro` | 40 per seat |

These values are version-sensitive provider configuration, not a timeless API guarantee. Preserve the distinction between the two Pro IDs; current public pricing alone would lose legacy-plan information. The Provider tier's inclusion above reproduces deployed Studio behavior, even though public documentation describes it as pay-as-you-go.

## Data and authentication

The [Studio billing hook](https://commandcode.ai/assets/use-billing-data-BvgdWZuE.js) uses cookie-authenticated GET requests (`credentials: include`) to the internal credit and subscription routes. The subscription request includes `withPending=true`; organization and administrative target parameters are optional. The hook expects `credits.monthlyCreditsGranted` but tolerates its absence. The [route constants](https://commandcode.ai/assets/constants-aSptHSSc.js) separately declare `/alpha/billing/credits` and `/alpha/billing/subscriptions` for CLI use.

The investigation inspected installed Command Code CLI **1.53.1** (`dist/cli.mjs`) and exercised the following read-only live requests using the same locally configured Bearer credential that Buildmesh already consumes. Credentials and API account identifiers are omitted from this note.

| Request | Live observation |
| --- | --- |
| `GET https://api.commandcode.ai/alpha/billing/credits` | Successful; `credits` contains `monthlyCredits`, `purchasedCredits`, `freeCredits`, threshold fields; `windowLimits` contains five-hour and weekly usage/cap/reset fields. No monthly denominator or reset; `monthlyCreditsGranted` absent in the observed response. |
| `GET https://api.commandcode.ai/alpha/billing/subscriptions` | Successful using the same Bearer authentication; `data` includes active `status`, `planId`, `currentPeriodStart`, and `currentPeriodEnd`. |

For the observed GOAT subscription, monthly remaining was approximately 42.705 out of 70: **39% used**, with the next reset on **September 30, 2026**. This is a point-in-time observation, not a fixture to embed in production. The CLI's `fetchUsageSubscription` supplies the subscription endpoint, and its `getCreditDepletionPct` likewise calculates depletion from monthly credits and its plan table.

Confirmed: the CLI credential can supply the required subscription and current monthly balance without adding browser authentication. Not yet live-verified: whether the alpha credits route ever returns the optional grant, organization quantity semantics on alpha subscriptions, past-due and missing-subscription payloads, and future/unknown plan handling. Public browser assets establish intended Studio behavior but do not guarantee alpha/internal response parity.

## Proposed Buildmesh integration

### Follow-up: can the CLI API eliminate the plan table?

Tracing `/usage` in installed CLI 1.53.1 confirms `fetchUsageData` requests `/alpha/whoami?limits=1`, credits, subscriptions, then `/alpha/usage/summary?since=<currentPeriodStart>`. `projectUsageView` resolves the subscription through the bundled plan table for active subscriptions. Its overall credit percentage includes extra credits; it is not necessarily identical to Studio's monthly-only percentage. The CLI bundle does not reference `monthlyCreditsGranted` or `totalMonthlyCredits`.

The live summary response does expose `totalMonthlyCredits` and `periodBasis: "billing-period"`, both with and without the explicit period-start query. However, a paired credit/summary observation returned monthly remaining 42.7050065056 and monthly consumed 27.3817629569, totaling 70.0867694625 rather than the tier allowance of 70. This would yield approximately 39.0684% instead of the CLI plan-based 38.9928%. The reason for the discrepancy is unverified; consumed plus remaining is therefore not established as an exact replacement for the allowance.

Read-only requests to `https://api.commandcode.ai/internal/billing/credits` and `/internal/billing/subscriptions?withPending=true` returned HTTP 401 with the CLI Bearer credential. The corresponding `commandcode.ai` host paths returned 404. `/alpha/whoami?limits=1` succeeded but returned user identity and a null organization, without allowance fields. Thus no verified CLI-authenticated response currently supplies a monthly allowance or ready-made monthly percentage for this account. Prefer a verified API allowance if available, but do not replace the plan lookup with an assumed equivalence to usage totals.

### Authenticated browser verification

Using the user-authorized temporary Chrome session through Playwright CLI 0.1.19, inspected the actual usage page's successful internal billing responses on 2026-09-12. The credits response contained `monthlyCredits: 42.7050065056`, `purchasedCredits: 0`, `premiumMonthlyCredits: 0`, `opensourceMonthlyCredits: 42.7050065056`, and **`monthlyCreditsGranted: null`**. It contained no monthly cap or calculated monthly percentage. The subscription response supplied `planId: "individual-goat"`, `status: "active"`, `quantity: 1`, and `currentPeriodEnd: "2026-09-30T13:13:19.000Z"`, but no allowance field.

The rendered page displayed **Monthly Limit, 39%, Resets on Sep 30**. Combined with the deployed calculation inspected above, this confirms that this account uses Studio's bundled tier allowance of 70; the authenticated browser endpoint does not eliminate the plan-table dependency. The recommended CLI-authenticated integration remains sufficient for this account without adding browser-session authentication. The optional grant should still be accepted if the alpha API supplies it in future, but its presence must not be assumed.

The organization path is an explicit implementation assumption backed by the deployed Studio JavaScript rather than a live organization-account observation: Studio floors `quantity`, requires at least one seat for the base-tier multiplication, and lets a positive `monthlyCreditsGranted` override the seat-derived base through `max(grant, tier total)`. Buildmesh preserves those precedence rules and tests fractional grants, fractional quantities, and the grant-plus-seat case. An authenticated organization response should revalidate this if Command Code changes the alpha contract.

### Integration recommendation

The owning backend is `src-tauri/src/services/usage.rs`; `src/components/AppSettings/UsageRender.tsx` already renders generic usage windows with percentages and reset information. An additional `UsageWindow` labeled `Monthly` can use the existing wire type.

Keep Buildmesh's existing presentation: one decimal place for percentage and a local-time reset date/time. Match Studio's underlying calculation and reset instant; its whole-number rounding and UTC formatting do not require provider-specific rendering.

- Enrich the existing Command Code probe with the alpha subscription GET using the existing auth and HTTP conventions. Parse optional grant data and maintain a documented provider plan-table fallback matching Studio. Unknown plans should not produce an invented denominator.
- Derive Monthly from the monthly pool and billing-period end. Display it alongside five-hour and weekly bars. Keep purchased/free credits separate because they are not the monthly allowance; the official [pricing documentation](https://commandcode.ai/docs/resources/pricing-limits) distinguishes top-ups from resetting subscription credits.
- Replace the prominent combined balance with Monthly when the monthly data is valid; retain separately labeled additional credits when nonzero. Keep the existing balance as fallback when subscription enrichment is unavailable.
- Treat subscription lookup failure as partial failure: preserve successfully fetched five-hour/weekly windows and credit balance. A missing subscription must not turn valid credit data into a provider-wide error. Hide Monthly for the Studio `past_due` case, absent/unknown plans, and invalid totals.
- Verify the production parser and loopback HTTP boundary for a successful monthly response, subscription-only failure, malformed/unknown subscription, credit-pool separation, optional grant, and plan-specific totals. Verify visible 0%/100% states, reset display, and narrow layout through the existing usage-rendering seam. These are implementation acceptance criteria; the PR records executed checks and runtime evidence.

The hashed asset links record the deployed implementation inspected on this date; deployment may retire those URLs. No public, documented monthly-usage API schema was found. The implementation should therefore treat this as an observed CLI contract and retain graceful partial-data behavior.
