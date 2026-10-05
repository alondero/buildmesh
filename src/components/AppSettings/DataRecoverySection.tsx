import { useCallback, useEffect, useState } from 'react';
import * as api from '../../lib/tauri/stateRecovery';
import { openInFileManager } from '../../lib/tauri';
import type {
  StateExportResult,
  StateIntegrityReport,
  StateRecoveryInfo,
  StateRestorePlan,
  StateSnapshot,
} from '../../lib/tauri/stateRecovery';

// Settings > Data & Diagnostics (issue #1537).
//
// Three groups, in the order the user needs them:
//
// 1. **Check** — is the stored state healthy? The quick check is the cheap
//    answer; the full check is the one to reach for when something looks
//    wrong.
// 2. **Back up** — snapshot now, or export a portable copy. Export defaults to
//    redacted, and the list of what an export *leaves out* is rendered rather
//    than buried in a tooltip: a user handing this file to someone else needs
//    to know what they are not handing over.
// 3. **Restore** — pick a bundle, read what it contains, then stage it. The
//    two-step confirm is deliberate. A restore replaces the user's whole
//    state, and the staged plan is what makes that a decision rather than a
//    click.
//
// ## Why restore is described as "applies on restart"
//
// It is literally true: `stage_state_restore` verifies and stages, and
// `run_profile_startup` applies before any connection exists. The copy here
// matches the backend's contract rather than promising an in-place swap the
// app cannot safely perform against its own live writer.
//
// ## Failure handling
//
// Every action reports through one `error` slot rather than inline per-button
// messages, so a failure is always visible without the user hunting. A
// *cancelled* dialog resolves to `null` and is explicitly not an error.

type Busy = 'none' | 'quick' | 'full' | 'snapshot' | 'export' | 'inspect' | 'stage' | 'cancel' | 'open';

// ## Tolerant reads
//
// The Tauri UI mock resolves any command it has no fixture for as `null`, on
// purpose, so a screen reading a command it does not know "renders empty
// instead of throwing" (see scripts/ui-mock/tauri-mock.mjs). Honouring that
// contract matters beyond the harness: `Promise.all` over a pane's initial
// reads resolves to `null` for anything unmocked, and a bare
// `snapshots.length` on that value takes down the whole Settings modal
// through the app's error boundary — losing the dialog, not just the pane.
// `coerceSnapshots` keeps a bad read to an empty list; `info` is read through
// optional chaining already.
const coerceSnapshots = (value: unknown): StateSnapshot[] =>
  Array.isArray(value) ? value : [];

export function DataRecoverySection() {
  const [info, setInfo] = useState<StateRecoveryInfo | null>(null);
  const [snapshots, setSnapshots] = useState<StateSnapshot[]>([]);
  const [report, setReport] = useState<StateIntegrityReport | null>(null);
  const [plan, setPlan] = useState<StateRestorePlan | null>(null);
  const [exported, setExported] = useState<StateExportResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Busy>('none');
  // Default on. There is deliberately no way to turn redaction off from the
  // Settings pane — see the note in `services/state_recovery/mod.rs` on why
  // exporting the LAN root CA key has no toggle.
  const [redact, setRedact] = useState(true);

  const refresh = useCallback(async () => {
    const [nextInfo, nextSnapshots] = await Promise.all([
      api.getStateRecoveryInfo(),
      api.listStateSnapshots(),
    ]);
    setInfo(nextInfo ?? null);
    setSnapshots(coerceSnapshots(nextSnapshots));
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [nextInfo, nextSnapshots] = await Promise.all([
          api.getStateRecoveryInfo(),
          api.listStateSnapshots(),
        ]);
        if (cancelled) return;
        setInfo(nextInfo ?? null);
        setSnapshots(coerceSnapshots(nextSnapshots));
      } catch (e) {
        if (!cancelled) setError(describe(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const run = useCallback(
    async <T,>(what: Busy, action: () => Promise<T>): Promise<T | null> => {
      setBusy(what);
      setError(null);
      try {
        return await action();
      } catch (e) {
        setError(describe(e));
        return null;
      } finally {
        setBusy('none');
      }
    },
    [],
  );

  const handleQuickCheck = () =>
    void run('quick', async () => {
      setReport(await api.checkStateIntegrity(false));
    });

  const handleFullCheck = () =>
    void run('full', async () => {
      setReport(await api.checkStateIntegrity(true));
    });

  const handleSnapshot = () =>
    void run('snapshot', async () => {
      const created = await api.createStateSnapshot();
      await refresh();
      setReport(null);
      setError(null);
      setPlan(null);
      return created;
    });

  const handleExport = () =>
    void run('export', async () => {
      const result = await api.exportState(redact);
      // A cancelled picker is not a failure.
      if (!result) return null;
      setExported(result);
      setError(null);
      return result;
    });

  const handleInspect = () =>
    void run('inspect', async () => {
      const inspected = await api.inspectStateBundle();
      setPlan(inspected);
      if (!inspected) setError(null);
      return inspected;
    });

  const handleStage = () =>
    void run('stage', async () => {
      const staged = await api.stageStateRestore();
      if (!staged) {
        setError(null);
        return null;
      }
      setPlan(staged);
      await refresh();
      setExported(null);
      setReport(null);
      setError(null);
      return staged;
    });

  const handleCancelRestore = () =>
    void run('cancel', async () => {
      await api.cancelStateRestore();
      setPlan(null);
      await refresh();
    });

  const handleOpenFolder = () =>
    void run('open', async () => {
      if (!info) return null;
      await openInFileManager(info.app_data_dir);
      return info.app_data_dir;
    });

  const disabled = busy !== 'none';
  const notice = info?.notice ?? null;

  return (
    <div className="pt-6 border-t border-border-subtle space-y-6" data-testid="settings-data-recovery">
      <div>
        <h3 className="text-lg font-medium text-text-secondary">Data &amp; Diagnostics</h3>
        <p className="text-sm text-text-muted mt-1">
          Back up, check, and restore the Meshes, Agent Nodes, Circuits, and preferences Buildmesh
          has stored on this device.
        </p>
      </div>

      {error && (
        <p
          className="text-sm text-status-error"
          role="alert"
          data-testid="settings-data-recovery-error"
        >
          {error}
        </p>
      )}

      {notice && (
        <div
          className="text-sm text-status-warning space-y-1"
          role="status"
          data-testid="settings-data-recovery-notice"
        >
          <p>{notice.message}</p>
          {notice.snapshot_path && (
            <p className="text-text-muted">A copy was kept at {notice.snapshot_path}</p>
          )}
        </div>
      )}

      {info?.pending_restore && (
        <div
          className="text-sm text-status-warning space-y-2"
          role="status"
          data-testid="settings-data-recovery-pending"
        >
          <p>A restore is staged and will be applied the next time Buildmesh starts.</p>
          <button
            type="button"
            onClick={handleCancelRestore}
            disabled={disabled}
            className="px-3 py-2 text-base text-text-secondary hover:text-text-primary disabled:opacity-50"
            data-testid="settings-data-recovery-cancel-restore"
          >
            Cancel the staged restore
          </button>
        </div>
      )}

      {/* --- Check ------------------------------------------------------ */}
      <section className="space-y-2" data-testid="settings-data-recovery-check">
        <h4 className="text-base font-medium text-text-primary">Check stored state</h4>
        <p className="text-sm text-text-muted">
          Reports problems without changing anything. Use the full check if the quick one is not
          enough.
        </p>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={handleQuickCheck}
            disabled={disabled}
            className="px-4 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            data-testid="settings-data-recovery-quick-check"
          >
            {busy === 'quick' ? 'Checking…' : 'Run quick check'}
          </button>
          <button
            type="button"
            onClick={handleFullCheck}
            disabled={disabled}
            className="px-4 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            data-testid="settings-data-recovery-full-check"
          >
            {busy === 'full' ? 'Checking…' : 'Run full check'}
          </button>
        </div>
        {report && (
          <p
            className={`text-sm ${report.ok ? 'text-status-success' : 'text-status-error'}`}
            aria-live="polite"
            data-testid="settings-data-recovery-report"
          >
            {report.message}
          </p>
        )}
      </section>

      {/* --- Back up ---------------------------------------------------- */}
      <section className="space-y-2" data-testid="settings-data-recovery-backup">
        <h4 className="text-base font-medium text-text-primary">Back up</h4>
        <p className="text-sm text-text-muted">
          Buildmesh keeps the newest {info?.retention ?? 3} automatic snapshots and takes one
          automatically before it upgrades your stored data.
          {info ? ` You currently have ${info.snapshot_count}.` : ''}
        </p>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={handleSnapshot}
            disabled={disabled}
            className="px-4 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            data-testid="settings-data-recovery-snapshot"
          >
            {busy === 'snapshot' ? 'Saving…' : 'Create snapshot'}
          </button>
          <button
            type="button"
            onClick={handleExport}
            disabled={disabled}
            className="px-4 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            data-testid="settings-data-recovery-export"
          >
            {busy === 'export' ? 'Exporting…' : 'Export a copy…'}
          </button>
        </div>

        <label className="flex items-center gap-3 text-sm text-text-primary cursor-pointer">
          <input
            type="checkbox"
            checked={redact}
            onChange={e => setRedact(e.target.checked)}
            disabled={disabled}
            className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
            data-testid="settings-data-recovery-redact"
          />
          <span>Leave credentials out of the export (recommended)</span>
        </label>
        <p className="text-xs text-text-muted">
          An export never contains provider API keys, the remote-access token, paired devices, the
          LAN HTTPS certificate and its private key, or terminal transcripts. Turning this off keeps
          your API keys and tokens in the file.
        </p>

        {exported && (
          <div
            className="text-sm text-text-muted space-y-1"
            data-testid="settings-data-recovery-export-result"
          >
            <p className="text-status-success">
              Exported {exported.sections.join(' and ')} to {exported.path}.
            </p>
            <p>Not included: {exported.omitted.join('; ')}.</p>
          </div>
        )}

        {snapshots.length > 0 && (
          <ul className="text-xs text-text-muted space-y-1" data-testid="settings-data-recovery-snapshot-list">
            {snapshots.map(snapshot => (
              <li key={snapshot.path} className="flex justify-between gap-4">
                <span>
                  {snapshot.created_at ? snapshot.created_at.replace('T', ' ').replace('Z', ' UTC') : snapshot.file_name}
                </span>
                <span>
                  {snapshot.kind} · v{snapshot.schema_version} · {formatBytes(snapshot.size_bytes)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* --- Restore ---------------------------------------------------- */}
      <section className="space-y-2" data-testid="settings-data-recovery-restore">
        <h4 className="text-base font-medium text-text-primary">Restore</h4>
        <p className="text-sm text-text-muted">
          Replaces your current state with one from a Buildmesh export or snapshot. Buildmesh keeps
          a copy of what you have now, so a restore can be undone.
        </p>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={handleInspect}
            disabled={disabled}
            className="px-4 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            data-testid="settings-data-recovery-inspect"
          >
            {busy === 'inspect' ? 'Reading…' : 'Choose a file…'}
          </button>
          {plan && !info?.pending_restore && (
            <button
              type="button"
              onClick={handleStage}
              disabled={disabled}
              className="px-4 py-2 bg-status-error text-bg-card font-medium text-base rounded-md hover:opacity-90 disabled:opacity-50"
              data-testid="settings-data-recovery-stage"
            >
              {busy === 'stage' ? 'Staging…' : 'Restore this state'}
            </button>
          )}
        </div>

        {plan && (
          <div
            className="text-sm space-y-2 border border-border-subtle rounded-md p-3"
            data-testid="settings-data-recovery-plan"
          >
            <p className="text-text-primary">
              {plan.rollback_snapshot
                ? 'Your current state will be kept as a snapshot first.'
                : 'There is no current state to keep as a snapshot.'}
            </p>
            <ul className="text-text-muted list-disc list-inside space-y-1">
              {plan.warnings.map(warning => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
            {plan.requires_restart && (
              <p className="text-status-warning">
                Buildmesh must restart to finish this. Nothing changes until then.
              </p>
            )}
          </div>
        )}
      </section>

      {/* --- Where the data lives --------------------------------------- */}
      <section className="space-y-2" data-testid="settings-data-recovery-location">
        <h4 className="text-base font-medium text-text-primary">Where this data lives</h4>
        <p className="text-sm text-text-muted break-all">{info?.app_data_dir ?? '…'}</p>
        <button
          type="button"
          onClick={handleOpenFolder}
          disabled={disabled || !info}
          className="px-4 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
          data-testid="settings-data-recovery-open-folder"
        >
          {busy === 'open' ? 'Opening…' : 'Open data folder'}
        </button>
      </section>
    </div>
  );
}

function describe(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
