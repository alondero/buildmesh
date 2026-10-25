import { useCallback, useEffect, useRef, useState } from 'react';
import { getUsage, isAuthError } from '../api';
import { AppBar, CenterNote } from '../ui';
import { useVisibilityPolling } from '../useVisibilityPolling';
import type { MobileProviderUsage } from '../../types/generated/MobileProviderUsage';
import type { UsageMeter } from '../../types/generated/UsageMeter';
import type { UsageAmount } from '../../types/generated/UsageAmount';
import { getUsageWindowPacePercent } from '../../lib/usagePace';

function Percent({ label, value, pace }: { label: string; value: number | null; pace?: number | null }) {
  return <div className="usage-window">
    <div className="usage-value"><span>{label}</span><span>{value == null ? 'Unavailable' : `${value.toFixed(1)}%`}</span></div>
    {value != null && <div className="usage-track" role="progressbar" aria-label={label} aria-valuenow={Math.min(100, Math.max(0, value))} aria-valuemin={0} aria-valuemax={100} aria-valuetext={`${value.toFixed(1)}% used`}>
      <div className="usage-fill" style={{ width: `${Math.min(100, Math.max(0, value))}%`, background: value > 80 ? 'var(--red)' : value > 60 ? 'var(--amber)' : 'var(--accent)' }} />
      {pace != null && <span className="usage-pace" style={{ left: `${pace}%` }} title={`Period ${pace.toFixed(0)}% elapsed`} />}
    </div>}
  </div>;
}

function Reset({ at }: { at: string | null }) {
  return at ? <p className="usage-note">Resets: {new Date(at).toLocaleString()}</p> : null;
}

function Amount({ amount, uncapped }: { amount: UsageAmount; uncapped: boolean }) {
  const fmt = (value: number) => `${value.toFixed(2)} ${amount.unit}`;
  return <div className="usage-window">
    <div className="usage-value"><span>Amount used</span><span>{fmt(amount.used)}</span></div>
    {!uncapped && amount.limit != null && <div className="usage-value"><span>Limit</span><span>{fmt(amount.limit)}</span></div>}
    {!uncapped && amount.remaining != null && <div className="usage-value"><span>Remaining</span><span>{fmt(amount.remaining)}</span></div>}
    {uncapped && <p>No individual limit</p>}
    {amount.usedPercent != null && <Percent label="Usage" value={amount.usedPercent} />}
    <Reset at={amount.resetsAt} />
  </div>;
}

function ExplicitMeter({ meter }: { meter: UsageMeter }) {
  switch (meter.state) {
    case 'metered': return <Amount amount={meter.amount} uncapped={false} />;
    case 'no_individual_limit': return <Amount amount={meter.amount} uncapped />;
    case 'unlimited': return <p>Unlimited</p>;
    case 'managed_externally': return <p>Managed by {meter.platform}</p>;
    case 'unavailable': return <p>Unavailable</p>;
  }
}

export function UsageCard({ row: { name, meter } }: { row: MobileProviderUsage }) {
  const usage = meter.usage;
  const balance = usage?.balance;
  return <section className="usage-card" aria-label={`${name} usage`}>
    <div className="usage-value"><h2>{name}</h2>{meter.usagePage && <a href={meter.usagePage.url} target="_blank" rel="noreferrer">Provider website ↗</a>}</div>
    {!meter.usageTracked ? <p>Usage not tracked</p>
      : !usage ? <p>Unable to load usage data</p>
      : usage.error ? <p role="alert">{usage.error}</p>
      : !usage.loggedIn ? <p>Sign in or update credentials on the desktop.</p>
      : <>
        {usage.windows.map((window, i) => <div key={i}><Percent label={window.label} value={window.usedPercent} pace={getUsageWindowPacePercent(window)} /><Reset at={window.resetsAt} /></div>)}
        {balance && <div className="usage-window">
          <div className="usage-value"><span>Balance remaining</span><span>{balance.currency} {balance.remaining.toFixed(2)}</span></div>
          {balance.monthlySpend != null && <div className="usage-value"><span>Spent this month</span><span>{balance.currency} {balance.monthlySpend.toFixed(2)}</span></div>}
        </div>}
        {usage.meters.map((item, i) => <ExplicitMeter key={i} meter={item} />)}
        {usage.windows.length === 0 && !balance && usage.meters.length === 0 && <p>Unavailable</p>}
        {usage.detail && <p className="usage-note">{usage.detail}</p>}
      </>}
    {meter.cachedAt != null && <p className="usage-note">Last known value · {new Date(meter.cachedAt * 1000).toLocaleString()}</p>}
  </section>;
}

export default function UsageScreen({ onBack, onAuthFailed }: { onBack: () => void; onAuthFailed: () => void }) {
  const [rows, setRows] = useState<MobileProviderUsage[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const revision = useRef(0);
  const refresh = useCallback(async (isLatest: () => boolean, signal?: AbortSignal, force = false) => {
    const request = ++revision.current;
    const current = () => request === revision.current && isLatest();
    setBusy(true);
    try {
      const next = await getUsage(force, signal);
      if (!current()) return;
      setRows(next);
      setError(null);
    } catch (e) {
      if (!current()) return;
      if (isAuthError(e)) { onAuthFailed(); return; }
      setError(e instanceof Error ? e.message : 'Failed to load usage');
    } finally {
      if (current()) setBusy(false);
    }
  }, [onAuthFailed]);
  const manualController = useRef(new AbortController());
  useEffect(() => {
    const controller = new AbortController();
    manualController.current = controller;
    return () => { controller.abort(); };
  }, []);
  useVisibilityPolling(refresh, 60_000);
  return <div className="screen">
    <AppBar title="Usage" subtitle="Provider usage on your desktop" onBack={onBack}>
      <button className="btn-ghost" disabled={busy} onClick={() => {
        const signal = manualController.current.signal;
        void refresh(() => !signal.aborted, signal, true);
      }}>{busy ? 'Refreshing…' : 'Refresh'}</button>
    </AppBar>
    {error && <div className="usage-error" role="alert">{error}{rows !== null && ' — showing last known data'}</div>}
    <div className="list-scroll" aria-busy={busy}>
      {rows === null ? <CenterNote>{busy ? 'Loading usage…' : 'Could not load usage. Tap Refresh to try again.'}</CenterNote>
        : rows.length === 0 ? <CenterNote>No usage meters available. Enable providers and sign in on the desktop.</CenterNote>
        : <div className="usage-list">{rows.map(row => <UsageCard key={row.meter.provider} row={row} />)}</div>}
    </div>
  </div>;
}
