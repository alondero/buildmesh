/**
 * AutopilotProbeTab — the Probe Panel's Autopilot tab (wayfinder #990,
 * ticket #994).
 *
 * Dedicated configure + monitor surface for the per-mesh Autopilot
 * feature, born out of map decision #5: instead of crowding
 * `MeshPropertiesTab` with the looping config, both the Issue-Driven
 * and Looping Autopilot modes live here. The mode toggle (`Issue-Driven`
 * vs `Looping`) is the `autopilot_mode` discriminator written through
 * `update_mesh_loop_config`; the controls underneath flip with it.
 *
 * What lives here today (everything is backed by `update_mesh_loop_config`
 * — one atomic write, mirroring `update_mesh_autopilot`'s shape):
 *
 *   - Mode toggle: `autopilot_mode` (Issue-Driven | Looping). A mode flip
 *     persists atomically with the current loop form values, so toggling
 *     while the user has typed prompts and caps doesn't drop the rest.
 *
 *   - Looping section (visible when `mode === 'looping'`):
 *     * Initial prompt textarea — required for the loop to do anything
 *       (`None` ⇒ daemon stays idle per the `MeshRow::loop_initial_prompt`
 *       doc comment).
 *     * Suffix prompt textarea — optional, injected after the standard
 *       `finish.md` wrap-up verification passes.
 *     * Max iterations number input — blank means continuous;
 *       backend enforces >=1 when Some.
 *     * Pause between iterations (seconds) — >=0.
 *     * Auto-pause after N consecutive failures (>=0, 0 = off).
 *     * Worktree override toggle — edits `mesh.use_worktree` via
 *       `update_mesh_use_worktree` (a separate column, written on its
 *       own). Issue-driven autopilot always uses a worktree regardless;
 *       looping respects this toggle.
 *
 *   - Issue-driven section: an explanatory paragraph plus a pointer
 *     to Mesh Properties where the trigger label, concurrency limit,
 *     provider, and PR action are still configured (ticket #481).
 *     Migrating those controls here is a future ticket; keeping them
 *     where they are avoids expanding the scope of #994.
 *
 * Status badge / Start-Pause-Stop (ticket #994 wording)
 * -----------------------------------------------------
 * The badge region renders the four ticket-mandated states (Active N /
 * Paused / Idle / Stopped) with the appropriate styling, plus the
 * Start/Pause/Stop action buttons. Until the loop scheduler
 * (wayfinder #990 ticket #992) lands there is no runtime surface to
 * read — the badge can only honestly report `Idle`, and the three
 * action buttons stay disabled with a tooltip explaining why. The
 * composition (badge + three actions + a small hint line) is laid out
 * first so #992 swaps the data source in without rearranging the tab.
 *
 * Reactivity model: matches `MeshPropertiesTab` (load on mesh change,
 * save indicator on every write, mesh-switch guard, prompts save on
 * blur, numeric fields save on blur with client-side validation so a
 * garbage keystroke shows up in the SaveIndicator instead of an
 * IPC round-trip rejection).
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useSaveStatus } from '../../hooks/useSaveStatus';
import {
  getLoopStatus,
  getMeshProperties,
  setMeshAutopilotEnabled,
  updateMeshLoopConfig,
  updateMeshUseWorktree,
} from '../../lib/tauri';
import type { MeshRow } from '../../types/generated/MeshRow';
import type { AutopilotMode } from '../../types/generated/AutopilotMode';
import type { LoopStatusDto } from '../../types/generated/LoopStatus';
import { LoadingState } from '../shared/Spinner';
import { SaveIndicator } from '../shared/SaveIndicator';
import { ProbeTabBody } from './ProbeTabBody';
import { Field } from './Field';

interface ModeOption {
  value: AutopilotMode;
  label: string;
}

const AUTOPILOT_MODE_OPTIONS: ModeOption[] = [
  { value: 'issue_driven', label: 'Issue-Driven' },
  { value: 'looping', label: 'Looping' },
];

/** Runtime loop status — the badge states reachable in the Start/Stop MVP
 *  (ticket #994). Derived from the backend `LoopStatusDto` via [`toLoopStatus`]:
 *  a live iteration → `active`; enabled but idle → `idle`; disabled → `stopped`.
 *  (There is no `paused` state — that needs a distinct DB column and is a
 *  deferred follow-up; Stop simply disables the loop.) */
export type LoopStatus =
  | { kind: 'active'; iteration: number }
  | { kind: 'idle' }
  | { kind: 'stopped' };

/** Per-variant colour classes keyed by `kind`. The label needs more shape
 *  per variant (the `active` iteration count), so the mapping is a small
 *  function below rather than a table of typed label-thunks — TS can't
 *  reconcile per-variant label signatures with a `LoopStatus` argument
 *  at the call site, but it narrows `status` for free inside a `switch`. */
const LOOP_STATUS_CLASSES: Record<LoopStatus['kind'], string> = {
  active: 'bg-accent-cyan/10 text-accent-cyan border-accent-cyan/30',
  idle: 'bg-bg-card text-text-muted border-border-subtle',
  stopped: 'bg-status-error/10 text-status-error border-status-error/30',
};

/** Render the user-visible label for the loop status badge. The `switch`
 *  narrows `status` per-case, so `status.iteration` is in-scope only on the
 *  `active` arm without an `as` re-narrow. */
function loopStatusLabel(status: LoopStatus): string {
  switch (status.kind) {
    case 'active':
      return `Active loop iteration ${status.iteration}`;
    case 'idle':
      return 'Idle';
    case 'stopped':
      return 'Stopped';
  }
}

/** Map the backend `LoopStatusDto` onto the badge's discriminated union.
 *  A live iteration (`active_iteration != null`) wins regardless of the
 *  enabled flag — a Stop during a running iteration still shows `Active`
 *  until that iteration finishes on its own. Otherwise `enabled` ⇒ `idle`
 *  (loop on, between iterations), `!enabled` ⇒ `stopped`. */
function toLoopStatus(dto: LoopStatusDto): LoopStatus {
  if (dto.active_iteration !== null) {
    return { kind: 'active', iteration: dto.active_iteration };
  }
  return dto.enabled ? { kind: 'idle' } : { kind: 'stopped' };
}

/** Form state. Numeric loop caps live as strings so an empty input means
 *  continuous (`maxIterations`) or zero (the two non-null counters)
 *  rather than 0, and so a non-numeric keystroke can be caught locally
 *  and surfaced through the shared SaveIndicator instead of coerced
 *  silently. The boolean `useWorktree` is the mesh's own column,
 *  persisted through its dedicated command (not the loop config). */
interface AutopilotLoopForm {
  mode: AutopilotMode;
  initialPrompt: string;
  suffixPrompt: string;
  maxIterations: string;
  intervalSeconds: string;
  consecutiveFailures: string;
  useWorktree: boolean;
}

const blankLoopForm: AutopilotLoopForm = {
  mode: 'issue_driven',
  initialPrompt: '',
  suffixPrompt: '',
  maxIterations: '',
  intervalSeconds: '0',
  consecutiveFailures: '0',
  useWorktree: true,
};

interface CoerceOk {
  mode: AutopilotMode;
  initialPrompt: string | null;
  suffixPrompt: string | null;
  maxIterations: number | null;
  intervalSeconds: number;
  consecutiveFailures: number;
}

type CoerceResult =
  | { ok: true; value: CoerceOk }
  | { ok: false; reason: string };

/** Parse the optional `maxIterations` field. Empty = continuous
 *  (`Some(None)` to the backend); non-empty must be a whole number
 *  >=1 per `update_mesh_loop_config`'s validation. */
function parseMaxIterations(input: string):
  | { ok: true; value: number | null }
  | { ok: false; reason: string } {
  const trimmed = input.trim();
  if (trimmed === '') return { ok: true, value: null };
  if (!/^\d+$/.test(trimmed)) {
    return { ok: false, reason: 'Max iterations must be a whole number, or empty for continuous' };
  }
  const n = Number(trimmed);
  if (!Number.isInteger(n) || n < 1) {
    return { ok: false, reason: 'Max iterations must be a positive integer (>= 1), or empty for continuous' };
  }
  return { ok: true, value: n };
}

/** Parse the non-null counters (interval seconds, consecutive failures).
 *  Empty = 0; non-empty must be a whole number >=0 per the backend's
 *  validation. */
function parseNonNegativeInt(input: string, label: string):
  | { ok: true; value: number }
  | { ok: false; reason: string } {
  const trimmed = input.trim();
  if (trimmed === '') return { ok: true, value: 0 };
  if (!/^\d+$/.test(trimmed)) {
    return { ok: false, reason: `${label} must be a whole number (>= 0)` };
  }
  const n = Number(trimmed);
  if (!Number.isInteger(n) || n < 0) {
    return { ok: false, reason: `${label} must be >= 0` };
  }
  return { ok: true, value: n };
}

/** The single source of truth for "what should the tab save today?".
 *  The mode toggle flips `mode` and writes every column atomically
 *  (per `update_mesh_loop_config`'s contract); text fields save on
 *  blur with the freshly loaded or typed value, never a stale closure
 *  snapshot. Trimmed, blank prompts collapse to `null` so the loop
 *  scheduler's `None`-means-idle invariant matches the user's intent. */
function coerceLoopForm(next: AutopilotLoopForm): CoerceResult {
  const trimmedInitial = next.initialPrompt.trim();
  const trimmedSuffix = next.suffixPrompt.trim();
  const max = parseMaxIterations(next.maxIterations);
  if (!max.ok) return max;
  const interval = parseNonNegativeInt(next.intervalSeconds, 'Pause between iterations');
  if (!interval.ok) return interval;
  const failures = parseNonNegativeInt(next.consecutiveFailures, 'Auto-pause after consecutive failures');
  if (!failures.ok) return failures;
  return {
    ok: true,
    value: {
      mode: next.mode,
      initialPrompt: trimmedInitial === '' ? null : trimmedInitial,
      suffixPrompt: trimmedSuffix === '' ? null : trimmedSuffix,
      maxIterations: max.value,
      intervalSeconds: interval.value,
      consecutiveFailures: failures.value,
    },
  };
}

// Shared Tailwind class strings — copy-pasted from the surrounding
// Probe form idiom so the tab blends with `MeshPropertiesTab`.
const CONTROL_CLASS =
  'w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted/60 placeholder:italic focus:outline-none focus:border-accent-cyan';
const TEXTAREA_CLASS =
  `${CONTROL_CLASS} font-mono resize-y`;

export function AutopilotProbeTab() {
  const { activeMeshId } = useProbeContext();
  const [form, setForm] = useState<AutopilotLoopForm>(blankLoopForm);
  const [loading, setLoading] = useState(true);
  // Runtime loop status for the badge (ticket #994). `null` = not yet
  // fetched (or not in looping mode); the row renders a muted "Checking…"
  // placeholder until the first `get_loop_status` resolves.
  const [loopStatus, setLoopStatus] = useState<LoopStatus | null>(null);
  const saveStatus = useSaveStatus();
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Reset the SaveIndicator on mesh-switch so a stale "Save failed"
  // from the outgoing mesh doesn't bleed onto the incoming mesh's
  // form (same defensive pattern as `MeshPropertiesTab.tsx`).
  useEffect(() => {
    saveStatus.reset();
  }, [activeMeshId, saveStatus.reset]);

  // Mesh-switch guard for in-flight saves (review finding #1 from
  // `MeshPropertiesTab.tsx`) — capture `activeMeshId` at IPC start,
  // then drop the resolve/reject result if the user switched meshes
  // while it was in flight.
  const activeMeshIdRef = useRef(activeMeshId);
  useEffect(() => {
    activeMeshIdRef.current = activeMeshId;
  }, [activeMeshId]);

  /** IPC wrapper. Mirrors `wrappedSave` in `MeshPropertiesTab.tsx`:
   *  drives the SaveIndicator state machine, and on rejection surfaces
   *  the rejection's `.message` so the user sees an actionable hint
   *  instead of a silent failure (issue #729). The mesh-binding
   *  concern lives here because the hook itself is mesh-agnostic. */
  const wrappedSave = async (op: () => Promise<void>) => {
    const saveMeshId = activeMeshIdRef.current;
    saveStatus.start();
    try {
      await op();
      if (activeMeshIdRef.current !== saveMeshId) return;
      saveStatus.success();
    } catch (e) {
      if (activeMeshIdRef.current !== saveMeshId) {
        console.error('Loop config save failed after mesh switch:', e);
        return;
      }
      console.error('Loop config save failed:', e);
      saveStatus.fail(e);
    }
  };

  // Load the mesh's saved config every time the active mesh changes.
  // `getMeshProperties` returns the full MeshRow from v30, which already
  // carries every `loop_*` column AND `use_worktree` so this tab doesn't
  // need a second IPC trip on mount.
  useAsyncEffect((signal) => {
    if (activeMeshId === null) {
      setLoading(false);
      return;
    }
    setLoading(true);
    getMeshProperties(activeMeshId)
      .then((row: MeshRow) => {
        if (signal.aborted) return;
        setForm({
          mode: row.autopilot_mode,
          initialPrompt: row.loop_initial_prompt ?? '',
          suffixPrompt: row.loop_suffix_prompt ?? '',
          maxIterations:
            row.loop_max_iterations === null ? '' : String(row.loop_max_iterations),
          intervalSeconds: String(row.loop_interval_seconds),
          consecutiveFailures: String(row.loop_consecutive_failures),
          useWorktree: row.use_worktree,
        });
        setLoading(false);
      })
      .catch(() => {
        if (!signal.aborted) setLoading(false);
      });
  }, [activeMeshId]);

  /** Save the loop config atomically. Local validation failures (bad
   *  numeric input, etc.) skip the IPC and surface through the
   *  SaveIndicator directly — matching the issue #729 AC of "show the
   *  message AND keep the field's text" (we keep the text because we
   *  never mutate `form` on a validation error). */
  const saveLoopConfig = async (next: AutopilotLoopForm) => {
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    const result = coerceLoopForm(next);
    if (!result.ok) {
      saveStatus.fail(new Error(result.reason));
      return;
    }
    const v = result.value;
    await wrappedSave(() =>
      updateMeshLoopConfig(
        meshId,
        v.mode,
        v.initialPrompt,
        v.suffixPrompt,
        v.maxIterations,
        v.intervalSeconds,
        v.consecutiveFailures
      )
    );
  };

  /** Optimistic helper for the controls that write one column at a
   *  time (the segmented mode toggle). Sets the next form, then funnels
   *  it through `saveLoopConfig` so every column lands atomically. */
  const patchLoopConfig = async (patch: Partial<AutopilotLoopForm>) => {
    const next = { ...form, ...patch };
    setForm(next);
    await saveLoopConfig(next);
  };

  /** Persist the worktree toggle via the dedicated `update_mesh_use_worktree`
   *  IPC — `use_worktree` is NOT a loop-config column (issue #481's
   *  issue-driven autopilot force-override notwithstanding), so it
   *  doesn't funnel through `update_mesh_loop_config`. Mirrors the
   *  optimistic pattern from `MeshPropertiesTab.saveSandbox`. */
  const saveUseWorktree = async (value: boolean) => {
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    setForm((p) => ({ ...p, useWorktree: value }));
    await wrappedSave(() => updateMeshUseWorktree(meshId, value));
  };

  /** Fetch the live loop status and map it onto the badge union. Mesh-switch
   *  guarded (drop the result if the user changed meshes mid-flight). A fetch
   *  failure is logged, not surfaced through the SaveIndicator — the badge is
   *  read-only telemetry, not a save the user just triggered. */
  const refreshLoopStatus = useCallback(async () => {
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    try {
      const dto = await getLoopStatus(meshId);
      if (!mountedRef.current || activeMeshIdRef.current !== meshId) return;
      setLoopStatus(toLoopStatus(dto));
    } catch (e) {
      console.error('getLoopStatus failed:', e);
    }
  }, []);

  // Poll the loop status while the Looping section is showing. The poller's
  // own cadence is ~2 min, so a light 5s refetch is enough to move the badge
  // from Idle → Active loop iteration N shortly after a spawn without hammering
  // the DB. Cleared on mesh-switch / mode-flip / unmount.
  useEffect(() => {
    if (loading || form.mode !== 'looping' || activeMeshId === null) {
      setLoopStatus(null);
      return;
    }
    void refreshLoopStatus();
    const id = setInterval(() => void refreshLoopStatus(), 5000);
    return () => clearInterval(id);
  }, [loading, form.mode, activeMeshId, refreshLoopStatus]);

  /** Start/Stop the loop — flips `autopilot_enabled` via its narrow command,
   *  then immediately refetches the status so the badge reflects the new state
   *  without waiting for the next poll tick. Routed through `wrappedSave` so a
   *  rejection surfaces in the SaveIndicator (this IS a user-triggered write). */
  const setLoopEnabled = async (enabled: boolean) => {
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    await wrappedSave(() => setMeshAutopilotEnabled(meshId, enabled));
    await refreshLoopStatus();
  };

  return (
    <ProbeTabBody padding="p-3" className="space-y-4">
      <SaveIndicator
        status={saveStatus.status}
        error={saveStatus.error}
        onDismiss={saveStatus.reset}
        testId="autopilot-save-indicator"
      />

      {loading ? (
        <LoadingState />
      ) : (
        <>
          {/* Mode toggle — segmented control with the same visual idiom
              as `GitPullRequestsTab`'s open/closed filter. The label is a
              sibling <span> (not a Field/htmlFor pair) because the
              control below it is a button group, not a single
              labelled element. */}
          <div>
            <span className="block text-xs text-text-muted mb-1">
              Autopilot mode
            </span>
            <div className="flex shrink-0 rounded-md overflow-hidden border border-border-subtle w-fit">
              {AUTOPILOT_MODE_OPTIONS.map((o) => {
                const active = form.mode === o.value;
                return (
                  <button
                    key={o.value}
                    type="button"
                    onClick={() => void patchLoopConfig({ mode: o.value })}
                    aria-pressed={active}
                    className={`px-3 py-1 text-xs font-medium transition-colors ${
                      active
                        ? 'bg-accent-cyan/20 text-accent-cyan'
                        : 'text-text-muted hover:text-text-secondary hover:bg-bg-card'
                    }`}
                  >
                    {o.label}
                  </button>
                );
              })}
            </div>
          </div>

          {form.mode === 'looping' ? (
              <LoopingSection
                form={form}
                setForm={setForm}
                onSaveLoopConfig={saveLoopConfig}
                onSaveUseWorktree={saveUseWorktree}
                loopStatus={loopStatus}
                onSetLoopEnabled={setLoopEnabled}
                mountedRef={mountedRef}
              />
          ) : (
            <IssueDrivenSection />
          )}
        </>
      )}
    </ProbeTabBody>
  );
}

interface LoopingSectionProps {
  form: AutopilotLoopForm;
  setForm: React.Dispatch<React.SetStateAction<AutopilotLoopForm>>;
  /** Persist one or more loop-config columns atomically (every prompt and
   *  numeric input funnels here; see `saveLoopConfig` in the parent). */
  onSaveLoopConfig: (next: AutopilotLoopForm) => Promise<void>;
  /** Persist the mesh's `use_worktree` flag via its dedicated IPC —
   *  worktree lives on a different column. */
  onSaveUseWorktree: (value: boolean) => Promise<void>;
  /** Live loop status for the badge, or `null` while the first fetch is
   *  in flight (renders a muted placeholder). */
  loopStatus: LoopStatus | null;
  /** Start (`true`) / Stop (`false`) the loop by flipping `autopilot_enabled`. */
  onSetLoopEnabled: (enabled: boolean) => Promise<void>;
  mountedRef: React.MutableRefObject<boolean>;
}

function LoopingSection({
  form,
  setForm,
  onSaveLoopConfig,
  onSaveUseWorktree,
  loopStatus,
  onSetLoopEnabled,
  mountedRef,
}: LoopingSectionProps) {
  return (
    <div className="space-y-4">
      {/* Runtime loop status row + Start/Stop controls (ticket #994). The
          loop is DB-config-driven: Start flips `autopilot_enabled` on and the
          poller (`services::autopilot`) spawns iterations for this Looping mesh
          within ~2 min; Stop flips it off (a running iteration finishes on its
          own). The badge is derived from `get_loop_status` — the enabled flag +
          iteration ledger — so it reflects real runtime state, not a guess. */}
      <LoopStatusRow
        status={loopStatus}
        promptBlank={form.initialPrompt.trim() === ''}
        onStart={() => onSetLoopEnabled(true)}
        onStop={() => onSetLoopEnabled(false)}
      />

      <Field
        label="Initial prompt"
        htmlFor="ap-initial-prompt"
        hint="required — loop stays idle when blank"
      >
        <textarea
          id="ap-initial-prompt"
          rows={5}
          spellCheck={false}
          value={form.initialPrompt}
          onChange={(e) =>
            setForm((p) => ({ ...p, initialPrompt: e.target.value }))
          }
          onBlur={async (e) => {
            if (!mountedRef.current) return;
            await onSaveLoopConfig({ ...form, initialPrompt: e.target.value });
          }}
          placeholder="e.g., Ship the highest-priority issue from #backlog. Verify with the build, commit, push, and open a draft PR."
          className={TEXTAREA_CLASS}
        />
      </Field>

      <Field
        label="Suffix prompt"
        htmlFor="ap-suffix-prompt"
        hint="optional — injected after wrap-up verification"
      >
        <textarea
          id="ap-suffix-prompt"
          rows={3}
          spellCheck={false}
          value={form.suffixPrompt}
          onChange={(e) =>
            setForm((p) => ({ ...p, suffixPrompt: e.target.value }))
          }
          onBlur={async (e) => {
            if (!mountedRef.current) return;
            await onSaveLoopConfig({ ...form, suffixPrompt: e.target.value });
          }}
          placeholder="e.g., Now write a changelog entry and post it to #releases."
          className={TEXTAREA_CLASS}
        />
      </Field>

      <Field
        label="Max iterations"
        htmlFor="ap-max-iterations"
        hint="blank = continuous"
      >
        <input
          id="ap-max-iterations"
          type="number"
          min={1}
          step={1}
          value={form.maxIterations}
          onChange={(e) =>
            setForm((p) => ({ ...p, maxIterations: e.target.value }))
          }
          onBlur={async (e) => {
            if (!mountedRef.current) return;
            await onSaveLoopConfig({ ...form, maxIterations: e.target.value });
          }}
          placeholder="∞"
          className={CONTROL_CLASS}
        />
      </Field>

      <Field
        label="Pause between iterations"
        htmlFor="ap-interval-seconds"
        hint="seconds — 0 = immediately"
      >
        <input
          id="ap-interval-seconds"
          type="number"
          min={0}
          step={1}
          value={form.intervalSeconds}
          onChange={(e) =>
            setForm((p) => ({ ...p, intervalSeconds: e.target.value }))
          }
          onBlur={async (e) => {
            if (!mountedRef.current) return;
            await onSaveLoopConfig({ ...form, intervalSeconds: e.target.value });
          }}
          placeholder="0"
          className={CONTROL_CLASS}
        />
      </Field>

      <Field
        label="Auto-pause after consecutive failures"
        htmlFor="ap-consecutive-failures"
        hint="0 = off"
      >
        <input
          id="ap-consecutive-failures"
          type="number"
          min={0}
          step={1}
          value={form.consecutiveFailures}
          onChange={(e) =>
            setForm((p) => ({ ...p, consecutiveFailures: e.target.value }))
          }
          onBlur={async (e) => {
            if (!mountedRef.current) return;
            await onSaveLoopConfig({ ...form, consecutiveFailures: e.target.value });
          }}
          placeholder="0"
          className={CONTROL_CLASS}
        />
      </Field>

      {/* Worktree toggle — `use_worktree` is a mesh column edited via
          `update_mesh_use_worktree`, not a loop-config column. Issue-
          driven autopilot still forces worktrees on spawn
          (`services/autopilot.rs` overrides `use_worktree_override =
          Some(true)`), so the explanation should make that asymmetry
          visible. */}
      <div>
        <label
          htmlFor="ap-use-worktree"
          className="flex items-center gap-2 text-xs cursor-pointer"
        >
          <input
            id="ap-use-worktree"
            type="checkbox"
            checked={form.useWorktree}
            onChange={async (e) => {
              await onSaveUseWorktree(e.target.checked);
            }}
            className="accent-accent-cyan"
          />
          <span className="text-text-primary">
            Run loop iterations in a worktree
          </span>
        </label>
        <p className="mt-1 text-xs text-text-muted/70">
          Looping iterations read this setting on every spawn. Toggle off
          for repos where worktrees don&apos;t apply (e.g. game decompilation
          working directly on the head branch). Issue-driven autopilot
          always runs in a worktree — this toggle only affects looping.
        </p>
      </div>
    </div>
  );
}

interface LoopStatusRowProps {
  /** Live status, or `null` while the first fetch is in flight. */
  status: LoopStatus | null;
  /** Whether the initial prompt is blank — the loop stays idle when it is,
   *  so Start is disabled with an explanatory tooltip. */
  promptBlank: boolean;
  onStart: () => void;
  onStop: () => void;
}

function LoopStatusRow({ status, promptBlank, onStart, onStop }: LoopStatusRowProps) {
  // `stopped` (or not-yet-loaded) means the loop is off → Start is the live
  // action; any other state means it's on → Stop is the live action.
  const enabled = status !== null && status.kind !== 'stopped';
  const startDisabled = status === null || enabled || promptBlank;
  const stopDisabled = status === null || !enabled;
  const startTitle = promptBlank
    ? 'Add an initial prompt first — the loop stays idle without one.'
    : 'Start the loop — the poller spawns the first iteration within ~2 min.';

  return (
    <div className="rounded-md border border-border-subtle bg-bg-card/50 px-3 py-2 space-y-2">
      <div className="flex items-center gap-2 flex-wrap">
        {status === null ? (
          <span
            data-testid="loop-status-badge"
            data-status="loading"
            className="inline-flex items-center px-2 py-0.5 rounded-full border text-xs font-medium bg-bg-card text-text-muted border-border-subtle"
          >
            Checking…
          </span>
        ) : (
          <span
            data-testid="loop-status-badge"
            data-status={status.kind}
            className={`inline-flex items-center px-2 py-0.5 rounded-full border text-xs font-medium ${LOOP_STATUS_CLASSES[status.kind]}`}
          >
            {loopStatusLabel(status)}
          </span>
        )}
        <div className="flex-1" />
        <button
          type="button"
          disabled={startDisabled}
          onClick={onStart}
          title={startTitle}
          aria-label="Start loop"
          className="text-xs px-2 py-1 rounded-md border border-accent-cyan/40 text-accent-cyan hover:bg-accent-cyan/10 disabled:border-border-subtle disabled:text-text-muted/60 disabled:cursor-not-allowed disabled:hover:bg-transparent transition-colors"
        >
          Start
        </button>
        <button
          type="button"
          disabled={stopDisabled}
          onClick={onStop}
          title="Stop the loop — no new iterations spawn (a running one finishes)."
          aria-label="Stop loop"
          className="text-xs px-2 py-1 rounded-md border border-border-subtle text-text-secondary hover:bg-bg-card disabled:text-text-muted/60 disabled:cursor-not-allowed disabled:hover:bg-transparent transition-colors"
        >
          Stop
        </button>
      </div>
      <p className="text-2xs text-text-muted/70">
        Looping runs while enabled; Stop halts new iterations (a running
        iteration finishes on its own). Start/Stop take effect within ~2 min —
        the poller re-reads config each pass.
      </p>
    </div>
  );
}

function IssueDrivenSection() {
  return (
    <div className="space-y-2 text-xs text-text-muted">
      <p>
        Issue-driven autopilot polls GitHub for labelled issues and
        auto-spawns an Agent Node per issue that implements, verifies,
        and opens a PR.
      </p>
      <p>
        Configure its policy — trigger label, concurrency limit,
        provider override, and post-merge action — under{' '}
        <span className="text-text-primary">Autopilot Mode</span> in the{' '}
        <span className="text-text-primary">Mesh Properties</span> tab.
        Switching this toggle to Looping is non-destructive: the issue-
        driven policy stays in Mesh Properties until you change or
        delete it.
      </p>
      <p>
        The two modes share a single mesh — flipping the toggle just
        switches which controller (issue-driven poller vs looping
        scheduler) is active for this project.
      </p>
    </div>
  );
}
