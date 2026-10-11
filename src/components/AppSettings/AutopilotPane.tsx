/**
 * `AutopilotPane` — host-wide limits for Circuit (autopilot) agents. Today
 * that is the app-wide agent pool size; it has its own tab because it is a
 * different concern from appearance and quitting, and people reach for it far
 * less often than anything on General.
 *
 * The draft hydrates from the shared `preferences` payload and commits on
 * blur / Enter rather than per keystroke, so a half-typed "1" of "10" never
 * briefly caps the pool at 1. It reports a dirty site so the modal's nav rail
 * can mark this pane and the discard banner can intercept a stray close
 * (issue #730).
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import * as api from '../../lib/tauri';
import type { AppPreferences } from '../../types/generated/AppPreferences';
import { formatError } from '../../lib/errorUtils';
import { SettingsRow, SettingsSection } from './SettingsRow';
import { ResourceLoadStatus } from './ResourceLoadStatus';
import { useSettingsData } from './SettingsDataContext';

export function AutopilotPane() {
  const { resources, prefsLoaded, preferences, loadPreferences, retryResource, setError, siteDirtyChange } =
    useSettingsData();

  // Circuit agent pool size (app-wide cap on concurrent Circuit agents).
  // The draft is a string so the input can hold a cleared/in-progress
  // value; `''` means "no global cap".
  const [poolDraft, setPoolDraft] = useState('');
  const [poolSaving, setPoolSaving] = useState(false);
  // The preferences payload whose draft is currently rendered. Null until
  // the first load commits.
  const [adoptedPrefs, setAdoptedPrefs] = useState<AppPreferences | null>(null);
  // Last value confirmed saved (canonical string form), for rollback and
  // dirty comparison. A ref for the same closure-staleness reason as the
  // optimistically-rolled-back pickers (issue #581).
  const poolSavedRef = useRef('');

  // Seed the draft from the latest winning preferences read *during render*
  // rather than from an effect: the input is controlled by the draft and its
  // `disabled` gate flips in the same render as the resource status, so an
  // effect would leave one committed frame where the input is enabled and
  // still showing the previous value. Re-seeding on a post-save refresh is
  // also what picks up a backend normalise-on-write.
  if (preferences && preferences !== adoptedPrefs) {
    setAdoptedPrefs(preferences);
    setPoolDraft(preferences.circuit_agent_pool_size == null ? '' : String(preferences.circuit_agent_pool_size));
  }

  // The saved baseline is seeded from an EFFECT rather than written inside the
  // render-phase adoption above: writing a ref during render is impure, and a
  // double-invoked render writes twice. The effect runs before any user
  // interaction can blur or type, so the value is current by the time a commit
  // reads it.
  useEffect(() => {
    if (!preferences) return;
    poolSavedRef.current =
      preferences.circuit_agent_pool_size == null ? '' : String(preferences.circuit_agent_pool_size);
  }, [preferences]);

  const poolDirtyChange = useCallback(
    (dirty: boolean) => siteDirtyChange('circuit-agent-pool', dirty),
    [siteDirtyChange],
  );

  // Commit the Circuit pool-size draft (blur / Enter). `''` clears the
  // global cap; anything else is clamped to a non-negative integer (0 =
  // pause new Circuit spawns). Optimistic with rollback; the dirty site
  // clears optimistically too so a successful save never leaves a phantom
  // discard banner.
  const commitPoolSize = async () => {
    const trimmed = poolDraft.trim();
    const numeric = Number(trimmed);
    if (trimmed !== '' && Number.isNaN(numeric)) {
      // Unreachable via the DOM (type=number sanitises non-numeric input to
      // ''), but guards the IPC from ever carrying NaN if the input type
      // changes: revert to the saved value rather than sending garbage.
      setPoolDraft(poolSavedRef.current);
      siteDirtyChange('circuit-agent-pool', false);
      return;
    }
    const parsed = trimmed === '' ? null : Math.max(0, Math.floor(numeric));
    const canonical = parsed === null ? '' : String(parsed);
    setPoolDraft(canonical);
    if (canonical === poolSavedRef.current) {
      siteDirtyChange('circuit-agent-pool', false);
      return;
    }
    const previous = poolSavedRef.current;
    poolSavedRef.current = canonical;
    siteDirtyChange('circuit-agent-pool', false);
    setPoolSaving(true);
    setError(null);
    try {
      await api.setAppCircuitAgentPoolSize(parsed);
      // Issue #1534 (review round 5) — refresh via the loader so a
      // failed `get_app_preferences` (or a backend normalisation
      // e.g. server-side cap adjustment) surfaces in the preferences
      // banner rather than leaving the optimistic `circuit_agent_pool_size`
      // silently stale.
      await loadPreferences();
    } catch (e) {
      poolSavedRef.current = previous;
      setPoolDraft(previous);
      setError(formatError(e));
    } finally {
      setPoolSaving(false);
    }
  };

  return (
    <>
      {resources.preferences.status === 'failed' && (
        <ResourceLoadStatus
          resource="preferences"
          state={resources.preferences}
          onRetry={() => retryResource('preferences')}
        />
      )}

      <SettingsSection title="Circuit agents">
        <SettingsRow
          label="Circuit agent pool size"
          htmlFor="circuit-agent-pool-size"
          summary="Global cap on Circuit agents across all meshes."
          controlClassName="w-48 shrink-0"
          details={
            <>
              The most Circuit agents allowed to run at once across all meshes.
              Each mesh's run capacity is configured in Circuits. Leave empty for
              no global cap; 0 pauses new agent launches. Lowering the cap holds
              new launches until slots free up; running agents are retained.
            </>
          }
        >
          <input
            id="circuit-agent-pool-size"
            type="number"
            min={0}
            step={1}
            inputMode="numeric"
            aria-label="Circuit agent pool size"
            placeholder="No global cap"
            value={poolDraft}
            disabled={!prefsLoaded || poolSaving}
            onChange={e => {
              setPoolDraft(e.target.value);
              poolDirtyChange(e.target.value.trim() !== poolSavedRef.current);
            }}
            onBlur={commitPoolSize}
            onKeyDown={e => {
              if (e.key === 'Enter') commitPoolSize();
            }}
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          />
        </SettingsRow>
      </SettingsSection>
    </>
  );
}
