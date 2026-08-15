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
 *   - `UsagePanel`   — one provider's row on the glanceable surface
 *                      (icon + name + optional Refresh + meter body)
 *
 * What's NOT here: any edit affordance (enable toggle, credential editor,
 * Remove). Those live on the Settings-side `AccountCard`. This split is
 * the issue #601 contract — the Usage surface is read-only, the Settings
 * surface is config-only.
 */

import type { ProviderAccount, ProviderMeters } from '../../lib/tauri';
import type { UsageWindow, BillingBalance } from '../../lib/tauri';
import { ProviderIcon } from '../Providers/ProviderIcon';

/** A single subscription-quota window as a labeled fill bar. The "0%
 *  renders as a real figure" rule is the issue #537 regression — a `> 0`
 *  guard would wrongly render Antigravity Claude/GPT-OSS models as N/A.
 *  Sized for the probe dock (text-xs labels, a 6px bar) — the meters are
 *  glanceable, not a dashboard, so they match the dock's compact rhythm
 *  instead of the roomier Settings-modal scale they were ported from. */
export function UsageBar({ window }: { window: UsageWindow }) {
  const percent = window.usedPercent ?? 0;
  const remaining = Math.max(0, 100 - percent);
  const color = percent > 80 ? 'bg-status-error' : percent > 60 ? 'bg-status-warning' : 'bg-accent-cyan';
  const display = window.usedPercent != null ? `${remaining.toFixed(1)}% remaining` : 'N/A';
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
        <p className="text-2xs text-text-muted mt-1">
          Resets in {formatResetCountdown(window.resetsAt)}
        </p>
      )}
    </div>
  );
}

function formatResetCountdown(resetsAt: string): string {
  const seconds = Math.ceil((new Date(resetsAt).getTime() - Date.now()) / 1000);
  if (!Number.isFinite(seconds) || seconds <= 0) return 'now';
  const totalMinutes = Math.ceil(seconds / 60);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours > 0) {
    const minutePart = minutes === 0 ? '' : ` ${minutes}m`;
    return `${hours}h${minutePart}`;
  }
  return `${Math.max(1, minutes)}m`;
}

/** Cash-balance view for a pay-as-you-go account (issue #537). */
export function BalanceCard({ balance }: { balance: BillingBalance }) {
  const fmt = (n: number) => `${balance.currency} ${n.toFixed(2)}`;
  return (
    <div className="mt-2 space-y-1">
      <div className="flex justify-between text-xs">
        <span className="text-text-muted">Balance remaining</span>
        <span className="font-medium font-mono text-text-primary">{fmt(balance.remaining)}</span>
      </div>
      {balance.monthlySpend != null && (
        <div className="flex justify-between text-xs">
          <span className="text-text-muted">Spent this month</span>
          <span className="font-mono text-text-primary">{fmt(balance.monthlySpend)}</span>
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
      const missingCredential = !meter.usage.error
        || meter.usage.error.startsWith('No API key')
        || meter.usage.error.startsWith('No credential');
      return (
        <div>
          <p className={`text-xs ${missingCredential ? 'text-status-warning' : 'text-status-error'}`}>
            {missingCredential ? keyed ? 'No API key' : 'Not logged in' : meter.usage.error}
          </p>
          <p className="text-2xs text-text-muted mt-1">
            {keyed
              ? `Enter an API key for ${account.name} above`
              : account.id === 'codex'
                ? 'Run codex in a terminal to log in'
                : `Run the ${account.name} CLI login first`}
          </p>
        </div>
      );
    }
    if (meter.usage.error) return <p className="text-xs text-status-error">{meter.usage.error}</p>;
    // Render every Usage Meter the provider exposes — quota windows AND
    // a cash balance can both be present, so show all rather than
    // choosing one by billing mode (#574 AC3). Also unhides MiniMax's
    // quota bars that the old billing-mode XOR suppressed.
    const hasMeters = meter.usage.windows.length > 0 || meter.usage.balance != null;
    return (
      <div>
        {meter.usage.windows.map(w => (
          <UsageBar key={w.label} window={w} />
        ))}
        {meter.usage.balance && <BalanceCard balance={meter.usage.balance} />}
        {!hasMeters && <p className="text-2xs text-text-muted">No usage data</p>}
        {meter.usage.detail && <p className="text-2xs text-accent-cyan mt-2">{meter.usage.detail}</p>}
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
            className="ml-auto text-xs text-accent-cyan hover:text-accent-cyan/80"
          >
            Refresh
          </button>
        )}
      </div>

      {renderBody()}
    </div>
  );
}
