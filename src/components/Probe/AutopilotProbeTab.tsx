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
 *   - Issue-driven section (formerly a prose pointer to Mesh Properties
 *     folded in by ticket #1013): the master `Autopilot on` checkbox
 *     + four policy columns — trigger label, max concurrent nodes,
 *     autopilot provider, on-success action. All five columns persist
 *     atomically through one `update_mesh_autopilot` IPC call (matching
 *     the pre-#1013 contract from `MeshPropertiesTab`; the command's
 *     behaviour is unchanged, only its call site moved). The fields
 *     only render while the master toggle is on, mirroring the prior
 *     Mesh Properties UX; the four columns persist either way so the
 *     user can re-enable without losing their configuration. The
 *     Looping section's Start/Stop still uses the narrow
 *     `set_mesh_autopilot_enabled` IPC so it can't clobber the issue-
 *     driven policy columns.
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
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { groupByHarness } from '../../lib/groups';
import {
  getAutopilotCompatibility,
  getLoopStatus,
  getMeshProperties,
  listProviders,
  setMeshAutopilotEnabled,
  updateMeshAutopilot,
  updateMeshCircuitRunCapacity,
  updateMeshLoopConfig,
  updateMeshUseWorktree,
  type ProviderInfo,
} from '../../lib/tauri';
import type { MeshRow } from '../../types/generated/MeshRow';
import type { AutopilotMode } from '../../types/generated/AutopilotMode';
import type { AutopilotCompatibility } from '../../types/generated/AutopilotCompatibility';
import type { AutopilotCompatibilityReason } from '../../types/generated/AutopilotCompatibilityReason';
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

// Issue-driven Autopilot Policy (issue #481 / PRD #480, folded out of
// `MeshPropertiesTab` by ticket #1013). The four fields render in
// `IssueDrivenSection` of THIS tab (wayfinder map #990 decision #5 —
// "Looping & Autopilot configuration and status monitoring live in a
// dedicated tab on the Probe Panel"). Concurrency matches the backend's
// 1..=8 range enforced by `update_mesh_autopilot`; the action literal
// matches `commands/mesh_properties::update_mesh_autopilot`'s allowed
// list (draft_pr / pr / none — plain string, not a generated enum).
const AUTOPILOT_CONCURRENCY_OPTIONS = [1, 2, 3, 4, 5, 6, 7, 8];
const AUTOPILOT_ACTION_OPTIONS = [
  { value: 'draft_pr', label: 'Open draft PR (default)' },
  { value: 'pr', label: 'Open PR ready for review' },
  { value: 'none', label: 'Push only (no PR)' },
];
const DEFAULT_AUTOPILOT_TRIGGER_LABEL = 'buildmesh:run';

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

/** Translate a single `AutopilotCompatibilityReason` to user-facing copy
 *  (issue #1152). Returns an object with `headline` (always shown) and
 *  optional `remedy` (a concrete corrective action the user can take).
 *  The headline is what the Probe banner shows; the remedy is the
 *  secondary line that tells the user what to change.
 *
 *  Kept as a pure function so the formatter is unit-testable in
 *  isolation from the React tree, and so the wire-side Rust enum
 *  (each variant) maps 1:1 to a stable English string — no silent
 *  fall-throughs, no `as any`. */
export function compatibilityReasonCopy(
  reason: AutopilotCompatibilityReason
): { headline: string; remedy: string | null } {
  switch (reason.kind) {
    case 'no_resolved_harness':
      return {
        headline: 'No Agent Harness could be resolved.',
        remedy: 'Open App Settings → Providers and ensure a default is set.',
      };
    case 'unknown_harness':
      return {
        headline: `Agent Harness "${reason.harness_id}" is not installed.`,
        remedy: 'Pick an installed harness from the Spawn Option list.',
      };
    case 'plain_terminal':
      return {
        headline: 'Terminal is a plain shell, not an Agent Harness.',
        remedy: 'Pick a real Agent Harness (Claude Code, Codex, Agy, …).',
      };
    case 'missing_attention_hook':
      return {
        headline: `${reason.harness_id} does not install an attention hook.`,
        remedy: 'Pick an Agent Harness that signals "awaiting input" events.',
      };
    case 'worktree_disabled':
      return {
        headline: 'Worktrees are disabled on this mesh.',
        remedy: 'Enable worktrees in Project Settings → Worktrees.',
      };
    // Defensive default: the wire-side Rust enum is the source of truth
    // for `AutopilotCompatibilityReason`, but the formatter lives in the
    // frontend and can drift if a new variant is added without a
    // matching case here. Without a default, a future drift would crash
    // the banner render with `Cannot read properties of undefined
    // (reading 'headline')` and silently blank every reason row. The
    // fallback renders the variant name verbatim so the user still sees
    // a reason (the testid on the `<li>` keys off `reason.kind`, so the
    // per-reason DOM marker survives too).
    default:
      return {
        headline: `Reason "${(reason as { kind: string }).kind}" is not supported by this Agent Harness.`,
        remedy: null,
      };
  }
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

/** Issue-driven Autopilot Policy form state (issue #481, folded out of
 *  `MeshPropertiesTab` by ticket #1013). All five fields persist atomically
 *  via `update_mesh_autopilot` (one IPC write carries them all); the master
 *  `enabled` flag also writes through this command rather than the narrow
 *  `set_mesh_autopilot_enabled` so the four policy columns travel with it.
 *  Blank `triggerLabel` collapses to `null` at write time (the poller's
 *  default-label fallback applies); an unset `actionOnSuccess` defaults to
 *  `'draft_pr'` to match the spec's "Open draft PR" wording. */
interface IssueDrivenForm {
  enabled: boolean;
  triggerLabel: string;
  concurrencyLimit: number;
  provider: string;
  actionOnSuccess: string;
}

const blankIssueDrivenForm: IssueDrivenForm = {
  enabled: false,
  triggerLabel: '',
  concurrencyLimit: 2,
  provider: '',
  actionOnSuccess: 'draft_pr',
};

/** Per-mesh cap on concurrent *admitted* circuit runs (issue #1467).
 *  Distinct from `IssueDrivenForm.concurrencyLimit` (legacy agent-slot
 *  count) and persisted through its OWN narrow single-column IPC
 *  (`update_mesh_circuit_run_capacity` mirrors `set_mesh_autopilot_enabled`).
 *  The two field shapes stay separate: the legacy 5-column atomic
 *  `update_mesh_autopilot` write never sees `circuitRunCapacity`, so
 *  toggling the run cap can't clobber the user's autopilot policy.
 *  Default 2 unlocks the two-overlap PR-review acceptance criterion
 *  out of the box. */
const DEFAULT_CIRCUIT_RUN_CAPACITY = 2;

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
  // Issue-driven Autopilot Policy form (ticket #1013, moved out of
  // `MeshPropertiesTab` to keep this tab the single configure surface for
  // both modes). Persists through `update_mesh_autopilot` (atomic 5-field
  // write); the loop form's concerns stay separate.
  const [issueDrivenForm, setIssueDrivenForm] = useState<IssueDrivenForm>(
    blankIssueDrivenForm
  );
  // Circuit-run capacity (issue #1467) — its OWN state slot and its
  // OWN persist handler. Distinct from `issueDrivenForm` because the
  // legacy 5-column `update_mesh_autopilot` write must never carry the
  // circuit-run cap (the narrow IPC `update_mesh_circuit_run_capacity`
  // mirrors `set_mesh_autopilot_enabled`'s sibling shape).
  const [circuitRunCapacity, setCircuitRunCapacity] = useState<number>(
    DEFAULT_CIRCUIT_RUN_CAPACITY
  );
  // Provider catalogue for the Issue-driven "Autopilot provider" select.
  // Fetched once on mount; the `provider-list-changed` event keeps it
  // honest when Settings edits the catalogue. We don't ship a ProviderInfo
  // list in the MeshRow — the fetch is unavoidable.
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [loading, setLoading] = useState(true);
  // Runtime loop status for the badge (ticket #994). `null` = not yet
  // fetched (or not in looping mode); the row renders a muted "Checking…"
  // placeholder until the first `get_loop_status` resolves.
  const [loopStatus, setLoopStatus] = useState<LoopStatus | null>(null);
  // Autopilot compatibility verdict (issue #1152). `null` while the
  // first fetch is in flight — the children render an "unknown" banner
  // rather than blocking on the verdict. The verdict controls whether
  // the master "Autopilot on" checkbox + Start button are enabled.
  const [compatibility, setCompatibility] = useState<AutopilotCompatibility | null>(null);
  const saveStatus = useSaveStatus();
  const { reset: resetSaveStatus } = saveStatus;
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
  // `resetSaveStatus` is destructured so the deps array can include
  // it without a disable directive (PR #1635 review feedback).
  useEffect(() => {
    resetSaveStatus();
  }, [activeMeshId, resetSaveStatus]);

  // Mesh-switch guard for in-flight saves (review finding #1 from
  // `MeshPropertiesTab.tsx`) — capture `activeMeshId` at IPC start,
  // then drop the resolve/reject result if the user switched meshes
  // while it was in flight.
  const activeMeshIdRef = useRef(activeMeshId);
  useEffect(() => {
    activeMeshIdRef.current = activeMeshId;
  }, [activeMeshId]);

  // Provider catalogue for the issue-driven "Autopilot provider" select.
  // Mirrors `MeshPropertiesTab`'s pattern: fetch once on mount; the
  // `provider-list-changed` event keeps it honest. We deliberately
  // always fetch (the loop section doesn't need providers, but the
  // catalogue is tiny and a toggle from `looping` → `issue_driven`
  // shouldn't pay a stale-state penalty).
  const refreshProviders = useCallback(() => {
    listProviders()
      .then(setProviders)
      .catch(() => setProviders([]));
  }, []);
  useEffect(() => {
    refreshProviders();
  }, [refreshProviders]);
  useProviderListInvalidation(refreshProviders);

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
        // Issue-driven Autopilot Policy (ticket #1013): the four policy
        // columns plus `autopilot_enabled` live on this tab now. Blank
        // trigger-label / provider collapse from `null` → `''` for the
        // input's `value`; `actionOnSuccess` defaults to `'draft_pr'`
        // (matching the spec's "Open draft PR" copy) so a fresh mesh
        // doesn't show an empty select.
        setIssueDrivenForm({
          enabled: row.autopilot_enabled,
          triggerLabel: row.autopilot_trigger_label ?? '',
          concurrencyLimit: row.autopilot_concurrency_limit || 2,
          provider: row.autopilot_provider ?? '',
          actionOnSuccess: row.autopilot_action_on_success ?? 'draft_pr',
        });
        // Circuit-run capacity lives on its own state slice so the
        // legacy Issue-driven form never carries it (issue #1467).
        setCircuitRunCapacity(row.circuit_run_capacity || DEFAULT_CIRCUIT_RUN_CAPACITY);
        setLoading(false);
      })
      .catch(() => {
        if (!signal.aborted) setLoading(false);
      });
  }, [activeMeshId]);

  /** Refresh the Autopilot compatibility verdict for the active Mesh
   *  (issue #1152). Mesh-switch guarded (drop the result if the user
   *  changed meshes mid-flight). A fetch failure is logged but does NOT
   *  block the form — we keep the previous verdict in state rather than
   *  flapping the gate open. The banner is the user feedback path; the
   *  IPC error is not surfaced through SaveIndicator because a verdict
   *  fetch is read-only telemetry, not a user-triggered save.
   *
   *  Refresh triggers:
   *  - Mesh switch (the effect above re-mounts this effect via deps).
   *  - After any save that can flip the verdict (worktree toggle,
   *    `autopilot_provider` change, default-provider change). The save
   *    handlers below call `refreshCompatibility()` directly so the
   *    verdict updates without waiting for a subsequent mesh-switch. */
  const refreshCompatibility = useCallback(async () => {
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    try {
      const verdict = await getAutopilotCompatibility(meshId);
      if (!mountedRef.current || activeMeshIdRef.current !== meshId) return;
      setCompatibility(verdict);
    } catch (e) {
      console.error('getAutopilotCompatibility failed:', e);
    }
  }, []);

  useEffect(() => {
    void refreshCompatibility();
  }, [activeMeshId, refreshCompatibility]);

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
   *  optimistic pattern from `MeshPropertiesTab.saveSandbox`.
   *
   *  Refreshes the compatibility verdict after a successful write —
   *  flipping `use_worktree` to false makes the mesh incompatible, so
   *  the verdict flips from `allowed=true` to `WorktreeDisabled` and
   *  the banner / enable controls must reflect that. The backend
   *  (`update_mesh_use_worktree`) ALSO auto-disables a previously-enabled
   *  mesh in this case — the verdict refresh surfaces that auto-disable
   *  so the user sees the new state without a mesh-switch. */
  const saveUseWorktree = async (value: boolean) => {
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    setForm((p) => ({ ...p, useWorktree: value }));
    await wrappedSave(() => updateMeshUseWorktree(meshId, value));
    await refreshCompatibility();
  };

  /** Persist the issue-driven Autopilot Policy in one atomic write
   *  (ticket #1013, moved out of `MeshPropertiesTab`). The five fields
   *  — `enabled`, `triggerLabel`, `concurrencyLimit`, `provider`,
   *  `actionOnSuccess` — travel together so a partial-update can't leave
   *  the policy fields out of sync with the master enable flag. Blank
   *  `triggerLabel` / `provider` collapse to `null`; an unset
   *  `actionOnSuccess` defaults to `'draft_pr'` (the spec's chosen
   *  default action). Optimistic like `saveUseWorktree`.
   *
   *  Refreshes the compatibility verdict after a successful write — the
   *  `provider` column is one of the three Spawn Option layers, so a
   *  change can flip the verdict (issue #1152). Refreshing here also
   *  covers the case where the user disables Autopilot after a
   *  compatibility error: the verdict stays `allowed=false` (the
   *  Spawn Option is still incompatible) but the user can move on. */
  const saveIssueDriven = async (next: IssueDrivenForm) => {
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    await wrappedSave(() =>
      updateMeshAutopilot(
        meshId,
        next.enabled,
        next.triggerLabel.trim() || null,
        next.concurrencyLimit,
        next.provider || null,
        next.actionOnSuccess || null
      )
    );
    await refreshCompatibility();
  };

  /** Optimistic patch helper for the issue-driven controls (master
   *  enable + 4 fields). Sets the next form, then funnels it through
   *  `saveIssueDriven` so the five columns land atomically. Mirrors the
   *  loop-section `patchLoopConfig` shape verbatim. The form is
   *  intentionally narrow — `circuitRunCapacity` lives on a separate
   *  state slice with its own patch handler (`patchCircuitRunCapacity`)
   *  so this 5-column atomic write can never accidentally carry it. */
  const patchIssueDriven = async (patch: Partial<IssueDrivenForm>) => {
    const next = { ...issueDrivenForm, ...patch };
    setIssueDrivenForm(next);
    await saveIssueDriven(next);
  };

  /** Optimistic patch for the circuit-run capacity (issue #1467). Lives
   *  on its OWN state slice (`circuitRunCapacity`) and writes through
   *  its OWN narrow IPC — never touches the legacy 5-column
   *  `update_mesh_autopilot` atomic write, so adjusting the run cap
   *  can't clobber the user's autopilot policy. The optimistic
   *  `setCircuitRunCapacity(capacity)` runs BEFORE the IPC so the UI
   *  updates immediately; `wrappedSave` shows the save indicator on
   *  rejection so the user sees the row flip back. */
  const patchCircuitRunCapacity = async (capacity: number) => {
    setCircuitRunCapacity(capacity);
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    await wrappedSave(() => updateMeshCircuitRunCapacity(meshId, capacity));
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
   *  rejection surfaces in the SaveIndicator (this IS a user-triggered write).
   *
   *  Refetches the compatibility verdict too — the loop's Start action is the
   *  primary place a user tries to enable Autopilot, so the verdict gates
   *  whether the call can succeed. A rejection from the backend (incompatible
   *  Spawn Option) is surfaced via SaveIndicator and the verdict remains
   *  `allowed=false` so the UI keeps the controls disabled. */
  const setLoopEnabled = async (enabled: boolean) => {
    const meshId = activeMeshIdRef.current;
    if (meshId === null) return;
    await wrappedSave(() => setMeshAutopilotEnabled(meshId, enabled));
    await refreshLoopStatus();
    await refreshCompatibility();
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
                compatibility={compatibility}
                mountedRef={mountedRef}
              />
          ) : (
            <IssueDrivenSection
              form={issueDrivenForm}
              setForm={setIssueDrivenForm}
              onSaveIssueDriven={saveIssueDriven}
              onPatchIssueDriven={patchIssueDriven}
              providers={providers}
              compatibility={compatibility}
              mountedRef={mountedRef}
              circuitRunCapacity={circuitRunCapacity}
              onPatchCircuitRunCapacity={patchCircuitRunCapacity}
            />
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
  /** Autopilot compatibility verdict (issue #1152). `null` while the
   *  first fetch is in flight — the section renders the controls as if
   *  compatible (the backend will reject an incompatible Start). */
  compatibility: AutopilotCompatibility | null;
  mountedRef: React.MutableRefObject<boolean>;
}

function LoopingSection({
  form,
  setForm,
  onSaveLoopConfig,
  onSaveUseWorktree,
  loopStatus,
  onSetLoopEnabled,
  compatibility,
  mountedRef,
}: LoopingSectionProps) {
  // Issue #1152: Start is disabled when the verdict says incompatible.
  // Stop is *always* available so the user can always turn off Autopilot
  // (an enabled-and-now-incompatible mesh still needs a manual reset
  // until the scheduler pass auto-disables it; the Stop control here is
  // the synchronous affordance).
  const compatible = compatibility === null || compatibility.allowed;
  return (
    <div className="space-y-4">
      {/* Compatibility banner (issue #1152). Renders nothing when the
          verdict is unknown or allowed — only surfaces when there is
          something actionable for the user to see. Each reason renders
          as its own row so the user sees every gap, not just the first. */}
      <CompatibilityBanner compatibility={compatibility} />

      {/* Runtime loop status row + Start/Stop controls (ticket #994). The
          loop is DB-config-driven: Start flips `autopilot_enabled` on and the
          poller (`services::autopilot`) spawns iterations for this Looping mesh
          within ~2 min; Stop flips it off (a running iteration finishes on its
          own). The badge is derived from `get_loop_status` — the enabled flag +
          iteration ledger — so it reflects real runtime state, not a guess.
          Issue #1152: Start is gated on the compatibility verdict so an
          incompatible Spawn Option never gets turned on (the backend will
          reject anyway, but the UI disabling prevents the round-trip). */}
      <LoopStatusRow
        status={loopStatus}
        promptBlank={form.initialPrompt.trim() === ''}
        compatible={compatible}
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
  /** Whether the resolved Spawn Option is compatible with the Autopilot
   *  pipeline (issue #1152). `false` disables Start with an explanatory
   *  tooltip. Stop is always available regardless of compatibility — the
   *  user must always be able to turn the loop off. */
  compatible: boolean;
  onStart: () => void;
  onStop: () => void;
}

function LoopStatusRow({ status, promptBlank, compatible, onStart, onStop }: LoopStatusRowProps) {
  // `stopped` (or not-yet-loaded) means the loop is off → Start is the live
  // action; any other state means it's on → Stop is the live action.
  const enabled = status !== null && status.kind !== 'stopped';
  const startDisabled = status === null || enabled || promptBlank || !compatible;
  const stopDisabled = status === null || !enabled;
  const startTitle = !compatible
    ? 'Autopilot cannot run on this Mesh — see the reason above. Fix the configuration first.'
    : promptBlank
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
          data-testid="autopilot-loop-start"
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

/** Compatibility banner (issue #1152). Renders nothing when the verdict is
 *  unknown (`null`) or compatible — only surfaces when there is something
 *  actionable for the user to see. Each reason renders as its own row so the
 *  user sees every gap, not just the first.
 *
 *  The banner is the single source of user-visible "why is this disabled?"
 *  copy; the LoopStatusRow + IssueDrivenSection only consume the verdict's
 *  boolean to gate their controls. */
function CompatibilityBanner({
  compatibility,
}: {
  compatibility: AutopilotCompatibility | null;
}) {
  if (compatibility === null || compatibility.allowed) return null;
  const headline = compatibility.explicit_autopilot_provider
    ? 'Autopilot selection is incompatible'
    : 'Default Autopilot Spawn Option is incompatible';
  return (
    <div
      data-testid="autopilot-compatibility-banner"
      role="status"
      className="rounded-md border border-status-warning/40 bg-status-warning/10 px-3 py-2 space-y-2"
    >
      <div className="flex items-baseline gap-2 flex-wrap">
        <span className="text-xs font-semibold text-status-warning">
          {headline}
        </span>
        {compatibility.resolved_spawn_option !== null && (
          <span className="text-2xs text-text-muted">
            (resolved: <code className="text-text-secondary">{compatibility.resolved_spawn_option}</code>)
          </span>
        )}
      </div>
      <ul className="space-y-1">
        {compatibility.reasons.map((reason, idx) => {
          const copy = compatibilityReasonCopy(reason);
          return (
            <li
              key={`${reason.kind}-${idx}`}
              data-testid={`autopilot-compatibility-reason-${reason.kind}`}
              className="text-xs text-text-secondary"
            >
              <span className="font-medium text-text-primary">
                {copy.headline}
              </span>
              {copy.remedy !== null && (
                <span className="block text-2xs text-text-muted/80 ml-0 mt-0.5">
                  → {copy.remedy}
                </span>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

interface IssueDrivenSectionProps {
  form: IssueDrivenForm;
  setForm: React.Dispatch<React.SetStateAction<IssueDrivenForm>>;
  /** Persist the four policy columns + master enable flag atomically.
   *  Carries the whole form (the `update_mesh_autopilot` IPC is one
   *  atomic write — every field travels with the master flag). */
  onSaveIssueDriven: (next: IssueDrivenForm) => Promise<void>;
  /** Optimistic patch helper for the simpler controls (master checkbox,
   *  selects). Sets the next form, then funnels through
   *  `onSaveIssueDriven`. */
  onPatchIssueDriven: (patch: Partial<IssueDrivenForm>) => Promise<void>;
  /** Provider catalogue for the "Autopilot provider" `<select>`. The
   *  parent fetches once on mount and re-fetches on
   *  `provider-list-changed`; this prop is read-only here. */
  providers: ProviderInfo[];
  /** Autopilot compatibility verdict (issue #1152). `null` while the
   *  first fetch is in flight. The master "Autopilot on" checkbox is
   *  gated on the verdict — enabling Autopilot with an incompatible
   *  Spawn Option is rejected by the backend, so the UI disablement
   *  prevents a guaranteed-fail round-trip. The four policy columns
   *  remain editable so the user can adjust the configuration that
   *  blocks enablement (issue #1152 AC #5). */
  compatibility: AutopilotCompatibility | null;
  mountedRef: React.MutableRefObject<boolean>;
  /** Circuit-run capacity (issue #1467) — its OWN state slice and
   *  patch handler, decoupled from `IssueDrivenForm` so the legacy
   *  5-column atomic `update_mesh_autopilot` write can never carry
   *  the run cap (the field persists through the narrow
   *  `update_mesh_circuit_run_capacity` IPC instead). */
  circuitRunCapacity: number;
  onPatchCircuitRunCapacity: (capacity: number) => Promise<void>;
}

function IssueDrivenSection({
  form,
  setForm,
  onSaveIssueDriven,
  onPatchIssueDriven,
  providers,
  compatibility,
  mountedRef,
  circuitRunCapacity,
  onPatchCircuitRunCapacity,
}: IssueDrivenSectionProps) {
  // Issue #1152: gating rules.
  // - `enableBlocked`: when the verdict says incompatible, the master
  //   "Autopilot on" checkbox can't be turned ON (backend would reject).
  //   Turning it OFF is always allowed — the user must always be able to
  //   disable Autopilot, even when the configuration is broken.
  // - `compatAllowToggle`: shorthand — true when the user CAN turn it on.
  const compatAllowToggle = compatibility === null || compatibility.allowed;
  // Once Autopilot is enabled, the master toggle's "off" branch is the
  // important one — flipping it off must stay possible regardless of the
  // verdict (so the user can always recover). When Autopilot is currently
  // disabled and the verdict is incompatible, the "on" branch is blocked.
  const enableBlocked = !form.enabled && !compatAllowToggle;
  const enableTitle = enableBlocked
    ? 'Autopilot cannot run on this Mesh — see the reason above. Fix the configuration first.'
    : undefined;
  return (
    <div className="space-y-3">
      {/* One-line intro — the loom sentence that used to live at the
          top of `MeshPropertiesTab`'s autopilot section. The four
          fields below carry the actual config; the intro just orients
          the user on this dedicated tab. */}
      <p className="text-xs text-text-muted">
        Issue-driven autopilot polls GitHub for labelled issues and
        auto-spawns an Agent Node per issue that implements, verifies,
        and opens a PR.
      </p>

      {/* Compatibility banner (issue #1152). See `CompatibilityBanner`
          in the Looping section above for the rationale — the banner
          is identical in shape so both modes use the same
          presentation. */}
      <CompatibilityBanner compatibility={compatibility} />

      {/* Master enable (issue #481). Owns the four policy columns in
          the same atomic write — unchecking fires
          `update_mesh_autopilot({ enabled: false, …4 fields })` so
          both halves stay in sync. Renamed from
          `MeshPropertiesTab`'s "Autopilot Mode" to avoid clashing
          with the `Autopilot mode` segmented toggle at the top of
          this tab.

          Issue #1152: blocked when the verdict is incompatible AND the
          user is trying to enable (i.e. currently disabled). The
          always-on `form.enabled === false` → `true` path is the one
          that's blocked; turning OFF is always permitted. */}
      <div>
        <label
          htmlFor="ap-policy-enabled"
          className="flex items-center gap-2 text-xs cursor-pointer"
        >
          <input
            id="ap-policy-enabled"
            type="checkbox"
            checked={form.enabled}
            disabled={enableBlocked}
            onChange={async (e) => {
              await onPatchIssueDriven({ enabled: e.target.checked });
            }}
            title={enableTitle}
            data-testid="autopilot-policy-enabled"
            className="accent-accent-cyan disabled:opacity-50 disabled:cursor-not-allowed"
          />
          <span className="text-text-primary">Autopilot on</span>
        </label>
        <p className="mt-1 text-xs text-text-muted/70">
          Turn the issue-driven poller on or off. The policy fields
          below apply only while this is checked; the four columns
          persist either way so you can re-enable without losing your
          configuration.
        </p>
      </div>

      {form.enabled && (
        <>
          <Field
            label="Trigger label"
            htmlFor="ap-policy-trigger-label"
            hint="default if blank"
          >
            <input
              id="ap-policy-trigger-label"
              type="text"
              value={form.triggerLabel}
              onChange={(e) =>
                setForm((p) => ({ ...p, triggerLabel: e.target.value }))
              }
              onBlur={async (e) => {
                if (!mountedRef.current) return;
                await onSaveIssueDriven({
                  ...form,
                  triggerLabel: e.target.value,
                });
              }}
              placeholder={DEFAULT_AUTOPILOT_TRIGGER_LABEL}
              className={CONTROL_CLASS}
            />
          </Field>

          <Field
            label="Max concurrent autopilot nodes"
            htmlFor="ap-policy-concurrency"
          >
            <select
              id="ap-policy-concurrency"
              value={form.concurrencyLimit}
              onChange={async (e) => {
                await onPatchIssueDriven({
                  concurrencyLimit: Number(e.target.value),
                });
              }}
              className={CONTROL_CLASS}
            >
              {AUTOPILOT_CONCURRENCY_OPTIONS.map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
          </Field>

          <Field
            label="Max concurrent circuit runs"
            htmlFor="ap-policy-circuit-run-capacity"
            hint="Per-mesh cap on admitted circuit runs (issue #1467). One slot per run, regardless of fan-out agent count. Default 2 lets two PR-review loops overlap without starving either reviewer."
          >
            <select
              id="ap-policy-circuit-run-capacity"
              value={circuitRunCapacity}
              onChange={async (e) => {
                await onPatchCircuitRunCapacity(Number(e.target.value));
              }}
              className={CONTROL_CLASS}
            >
              {AUTOPILOT_CONCURRENCY_OPTIONS.map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
          </Field>

          <Field
            label="Autopilot provider"
            htmlFor="ap-policy-provider"
          >
            <select
              id="ap-policy-provider"
              value={form.provider}
              onChange={async (e) => {
                await onPatchIssueDriven({ provider: e.target.value });
              }}
              className={CONTROL_CLASS}
            >
              <option value="">&lt;Mesh default&gt;</option>
              {groupByHarness(providers).map(([harnessId, group]) => {
                if (group.length === 1) {
                  return (
                    <option key={group[0].id} value={group[0].id}>
                      {group[0].label}
                    </option>
                  );
                }
                const native = group.find((p) => !p.is_proxied) ?? group[0];
                return (
                  <optgroup key={harnessId} label={native.label}>
                    {group.map((p) => (
                      <option key={p.id} value={p.id}>
                        {p.is_proxied
                          ? `  ${p.label} (via ${native.label})`
                          : p.label}
                      </option>
                    ))}
                  </optgroup>
                );
              })}
            </select>
          </Field>

          <Field label="On success" htmlFor="ap-policy-action">
            <select
              id="ap-policy-action"
              value={form.actionOnSuccess || 'draft_pr'}
              onChange={async (e) => {
                await onPatchIssueDriven({
                  actionOnSuccess: e.target.value,
                });
              }}
              className={CONTROL_CLASS}
            >
              {AUTOPILOT_ACTION_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </Field>
        </>
      )}
    </div>
  );
}
