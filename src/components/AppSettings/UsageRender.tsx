/**
 * `<UsageBar>` / `<BalanceCard>` / `<UsagePanel>` — the read-only Usage
 * Meter primitives shared by the Probe Panel's "Usage" tab (issue #601)
 * and the legacy Settings-side accounts panel.
 *
 * Extracted from `AppSettingsModal.tsx` (issue #601) so the glanceable
 * surface can live outside the Settings modal. Detection-gating logic
 * stays unchanged: native harnesses' subscription meters only show
 * when the harness is installed; keyed providers' meters only show
 * when the account is enabled (issue #574).
 *
 * What lives here:
 *   - `UsageBar`     — single `UsageWindow` → labeled fill bar (#537)
 *   - `BalanceCard`  — single `BillingBalance` → two-row wallet readout
 *   - `ExplicitUsageMeter` — capped, uncapped, unlimited, external, unavailable
 *   - `UsagePanel`   — one provider's row on the glanceable surface
 *                      (icon + name + optional Refresh + meter body)
 *
 * What's NOT here: any edit affordance (enable toggle, credential editor,
 * Remove). Those live on the Settings-side `AccountCard`. This split is
 * the issue #601 contract — the Usage surface is read-only, the Settings
 * surface is config-only.
 */

import type { ProviderAccount, ProviderMeters } from '../../lib/tauri';
import type { UsageWindow, BillingBalance, UsageAmount, UsageMeter } from '../../lib/tauri';
import { ProviderIcon } from '../Providers/ProviderIcon';

/** A single subscription-quota window as a labeled fill bar. The "0%
 *  renders as a real figure" rule is the issue #537 regression — a `> 0`
 *  guard would wrongly render Antigravity Claude/GPT-OSS models as N/A.
 *  Sized for the probe dock (text-xs labels, a 6px bar) — the meters are
 *  glanceable, not a dashboard, so they match the dock's compact rhythm
 *  instead of the roomier Settings-modal scale they were ported from. */
export function UsageBar({ window }: { window: UsageWindow }) {
  const percent = window.usedPercent ?? 0;
  const color = percent > 80 ? 'bg-status-error' : percent > 60 ? 'bg-status-warning' : 'bg-accent-cyan';
  const display = window.usedPercent != null ? `${percent.toFixed(1)}%` : 'Unavailable';
  return (
    <div className="mt-2 first:mt-0">
      <div className="flex justify-between items-baseline gap-2 text-xs mb-1">
        <span className="text-text-secondary truncate" title={window.label}>{window.label}</span>
        <span className="font-mono text-text-muted shrink-0">{display}</span>
      </div>
      <div className="h-1.5 bg-bg-card rounded-full overflow-hidden">
        <div
          className={`h-full ${color} rounded-full transition-[width] duration-300`}
          style={{ width: `${Math.min(percent, 100)}%` }}
        />
      </div>
      {window.resetsAt && (
        <p className="text-2xs text-text-muted mt-1 tabular-nums">Resets: {new Date(window.resetsAt).toLocaleString()}</p>
      )}
    </div>
  );
}

function formatUsageAmount(value: number, unit: string) {
  const formatted = value.toFixed(2);
  return /^[A-Z]{3}$/.test(unit) ? `${unit} ${formatted}` : `${formatted} ${unit}`;
}

function AmountRow({ label, value, unit }: { label: string; value: number; unit: string }) {
  return (
    <div className="flex flex-wrap justify-between gap-x-2 gap-y-0.5 text-xs">
      <span className="text-text-muted">{label}</span>
      <span className="font-mono text-text-primary break-words">
        {formatUsageAmount(value, unit)}
      </span>
    </div>
  );
}

function AmountMeter({ amount, uncapped }: { amount: UsageAmount; uncapped: boolean }) {
  return (
    <div
      className="space-y-1"
      data-usage-state={uncapped ? 'no_individual_limit' : 'metered'}
      data-testid={uncapped ? 'usage-state-no-individual-limit' : 'usage-state-metered'}
    >
      <AmountRow label="Amount used" value={amount.used} unit={amount.unit} />
      {!uncapped && amount.limit != null && (
        <AmountRow label="Limit" value={amount.limit} unit={amount.unit} />
      )}
      {!uncapped && amount.remaining != null && (
        <AmountRow label="Remaining" value={amount.remaining} unit={amount.unit} />
      )}
      {uncapped && <p className="text-xs text-text-secondary">No individual limit</p>}
      {amount.usedPercent != null && (
        <div className="pt-1">
          <div className="flex justify-between gap-2 text-xs mb-1">
            <span className="text-text-muted">Usage</span>
            <span className="font-mono text-text-primary shrink-0">
              {amount.usedPercent.toFixed(1)}%
            </span>
          </div>
          <div className="h-1.5 bg-bg-card rounded-full overflow-hidden">
            <div
              className="h-full bg-accent-cyan rounded-full transition-[width] duration-300"
              style={{ width: `${Math.min(Math.max(amount.usedPercent, 0), 100)}%` }}
            />
          </div>
        </div>
      )}
      {amount.resetsAt && (
        <p className="text-2xs text-text-muted pt-1 tabular-nums break-words">
          Resets: {new Date(amount.resetsAt).toLocaleString()}
        </p>
      )}
    </div>
  );
}

/** Explicit capped, uncapped, unlimited, externally managed, or unavailable meter. */
export function ExplicitUsageMeter({ meter }: { meter: UsageMeter }) {
  switch (meter.state) {
    case 'metered':
      return <AmountMeter amount={meter.amount} uncapped={false} />;
    case 'no_individual_limit':
      return <AmountMeter amount={meter.amount} uncapped />;
    case 'unlimited':
      return (
        <p className="text-xs text-text-secondary" data-usage-state="unlimited" data-testid="usage-state-unlimited">
          Unlimited
        </p>
      );
    case 'managed_externally':
      return (
        <p
          className="text-xs text-text-secondary break-words"
          data-usage-state="managed_externally"
          data-testid="usage-state-managed-externally"
        >
          Managed by {meter.platform}
        </p>
      );
    case 'unavailable':
      return (
        <p className="text-xs text-text-muted" data-usage-state="unavailable" data-testid="usage-state-unavailable">
          Unavailable
        </p>
      );
  }
}

/** Whether `BalanceCard` would render any row for a given balance.
 *  `BalanceCard` hides "Balance remaining" when exactly zero (a fresh
 *  wallet has nothing to compare against) and only renders "Spent this
 *  month" when monthly spend is set. Returns true if either row would
 *  appear. -0 collapses to 0 in JS so no special-case is needed; a
 *  negative remaining (overdrawn wallet) is informative and counts. */
export function isBalanceVisible(balance: BillingBalance): boolean {
  return balance.remaining !== 0 || balance.monthlySpend != null;
}

/** Cash-balance view for a pay-as-you-go account (issue #537).
 *  Returns null when nothing is renderable (e.g.
 *  `{ remaining: 0, monthlySpend: null }`) so the parent doesn't show
 *  an empty box. */
export function BalanceCard({ balance }: { balance: BillingBalance }) {
  if (!isBalanceVisible(balance)) return null;
  const fmt = (n: number) => `${balance.currency} ${n.toFixed(2)}`;
  const showRemaining = balance.remaining !== 0;
  const showSpend = balance.monthlySpend != null;
  return (
    <div className="mt-2 space-y-1">
      {showRemaining && (
        <div className="flex justify-between text-xs">
          <span className="text-text-muted">Balance remaining</span>
          <span className="font-medium font-mono text-text-primary">{fmt(balance.remaining)}</span>
        </div>
      )}
      {showSpend && (
        <div className="flex justify-between text-xs">
          <span className="text-text-muted">Spent this month</span>
          <span className="font-mono text-text-primary">{fmt(balance.monthlySpend!)}</span>
        </div>
      )}
    </div>
  );
}

/** One provider's read-only Usage Meter row on the glanceable surface.
 *  Pairs an account (for name + icon + enabled state) with the matching
 *  `ProviderMeters` row from `get_provider_meters`. The optional
 *  `onRefresh` shows a per-row Refresh button when the parent owns
 *  refresh state (the Probe tab does — its tab-level Refresh already
 *  re-fetches every row, but a row-level refresh is allowed for future
 *  "refresh just this provider" affordances). */
export function UsagePanel({
  account,
  meter,
  onRefresh,
}: {
  account: ProviderAccount;
  meter: ProviderMeters;
  onRefresh?: () => Promise<void> | void;
}) {
  // Native harnesses self-authenticate via their own CLI; keyed
  // providers (Claude-compatible) authenticate with an API key. The
  // branch drives the "Not logged in" vs "No API key" copy in the
  // logged-out state.
  const keyed = account.claude_compatible;

  const renderBody = () => {
    // Disabled-but-visible (e.g. the Settings-side AccountCard's row): no
    // meter, just a hint. The Probe Panel's UsageTab pre-filters disabled
    // accounts upstream, so this branch is unreachable from there; it
    // stays as a guard for any direct caller.
    if (!account.enabled) return <p className="text-xs text-text-muted">Disabled</p>;
    // Generic Model Provider without a fetcher: explicit, not an empty
    // gauge or misleading error (#574 AC4).
    if (!meter.usageTracked) {
      return (
        <div>
          <p className="text-xs text-text-muted">Usage not tracked</p>
          <p className="text-2xs text-text-muted mt-1">
            Buildmesh has no usage integration for {account.name}.
          </p>
        </div>
      );
    }
    // Defensive: backend should always populate `usage` for a tracked
    // provider; if it doesn't, surface a neutral placeholder.
    if (!meter.usage) return <p className="text-xs text-text-muted">Unable to load usage data</p>;
    if (!meter.usage.loggedIn) {
      return (
        <div>
          <p className="text-xs text-status-warning">{keyed ? 'No API key' : 'Not logged in'}</p>
          <p className="text-2xs text-text-muted mt-1">
            {keyed ? `Enter an API key for ${account.name} above` : `Run the ${account.name} CLI login first`}
          </p>
        </div>
      );
    }
    if (meter.usage.error) return <p className="text-xs text-status-error">{meter.usage.error}</p>;
    // Render every Usage Meter the provider exposes — quota windows AND
    // a cash balance can both be present, so show all rather than
    // choosing one by billing mode (#574 AC3). Also unhides MiniMax's
    // quota bars that the old billing-mode XOR suppressed.
    const explicitMeters = meter.usage.meters ?? [];
    // Mirror BalanceCard's render predicate via the shared helper so the
    // panel and the card cannot desynchronize. A balance with no
    // remaining (exact zero) and no monthly spend contributes nothing
    // and would be skipped by the BalanceCard null-return below; don't
    // count it as "has meters" or the panel would hide "Unavailable"
    // and show a blank box.
    const balanceRendersContent = meter.usage.balance != null
      && isBalanceVisible(meter.usage.balance);
    const hasMeters = meter.usage.windows.length > 0
      || balanceRendersContent
      || explicitMeters.length > 0;
    return (
      <div>
        {meter.usage.windows.map(w => (
          <UsageBar key={w.label} window={w} />
        ))}
        {meter.usage.balance && balanceRendersContent && <BalanceCard balance={meter.usage.balance} />}
        {explicitMeters.map((usageMeter, index) => (
          <div key={index} className="mt-2 first:mt-0">
            <ExplicitUsageMeter meter={usageMeter} />
          </div>
        ))}
        {!hasMeters && <p className="text-2xs text-text-muted">Unavailable</p>}
        {meter.usage.detail && <p className="text-2xs text-text-secondary mt-2">{meter.usage.detail}</p>}
      </div>
    );
  };

  return (
    <div
      className="border border-border-subtle rounded-lg p-3.5 bg-bg-card/30"
      data-testid={`usage-panel-${account.id}`}
    >
      <div className="flex items-center gap-2 mb-2">
        <ProviderIcon providerId={account.id} className="h-4 w-4" />
        <span className="text-sm font-medium text-text-primary truncate">{account.name}</span>
        {onRefresh && (
          <button
            type="button"
            onClick={async () => {
              // CLAUDE.md "user.click swallows async onClick rejections": the
              // parent passes an async fetcher; wrap in try/catch so a future
              // refactor that drops the parent's internal try/catch doesn't
              // surface as an unhandled rejection in the console.
              try {
                await onRefresh();
              } catch {
                // Parent owns error UI (toast / inline banner); the row's
                // own Refresh button just needs to not throw.
              }
            }}
            aria-label={`Refresh usage for ${account.name}`}
            className="ml-auto text-xs text-text-secondary hover:text-text-primary"
          >
            Refresh
          </button>
        )}
      </div>

      {renderBody()}
    </div>
  );
}
