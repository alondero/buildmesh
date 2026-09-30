/**
 * WorktreeStrategySection — the per-project worktree strategy controls
 * (issue #1460).
 *
 * These four controls used to live in the Worktree Manager tab, sitting
 * directly above the prune toolbar. That placement made a *configuration*
 * decision look like the first step of a *maintenance* action, and it is
 * the concrete reason the two destinations were undifferentiated: a user
 * changing where new agent worktrees are cut had to enter a
 * maintenance-heavy surface to do it. #1460 moves the block into Project
 * Settings as its own labelled section, and the Repository destination
 * keeps only health, recovery, and cleanup.
 *
 * The section is deliberately "dumb": it owns no state, so the
 * "form mirrors user intent" and "do not revert on save failure" guarantees
 * stay in one place (Project Settings' `wrappedSave` and the worktree
 * config handlers beside it). All saves fire on change through the parent's
 * typed wrappers — no raw `invoke`, which preserves the `tauri-ipc-seam`
 * ratchet at `tests/unit/tauri-ipc-seam.test.ts`.
 *
 * The wrapping `<label>` pattern (option label + nested input) is
 * intentional: it makes `getByLabelText('Use worktree')` resolve the
 * checkbox in the test harness without a separate `id`/`htmlFor` wire. Radio
 * options use the same pattern so `getByRole('radio', { name: /Fresh/ })`
 * finds them. The pool toggle follows suit.
 */

import { useEffect, useState } from 'react';

/**
 * The default worktree mode for a fresh agent node. Local pin that must
 * agree with `DEFAULT_WORKTREE_MODE` in `src-tauri/src/agent/spawn.rs`
 * (per the cross-language default coupling pattern — see ADR
 * follow-up). When the `meshes.worktree_mode` column is missing or
 * `null` on the wire, this is the value the form falls back to.
 */
export const DEFAULT_WORKTREE_MODE = 'branched';

/**
 * Wire-shape form values for the Starting point radio. The on-the-wire
 * value (passed to `update_worktree_base_ref`) is the `wire` field —
 * `'origin/main'` for fresh sessions, `'HEAD'` for resuming. The legacy
 * `MeshPropertiesPanel` (deleted in #380) used the same two options.
 */
export type BaseRefForm = 'fresh' | 'head';

const BASEREF_OPTIONS: { value: BaseRefForm; label: string; wire: 'origin/main' | 'HEAD' }[] = [
  { value: 'fresh', label: 'Fresh — start new session (origin/<default>)', wire: 'origin/main' },
  { value: 'head', label: 'Head — resume last session (HEAD)', wire: 'HEAD' },
];

/**
 * Wire-shape form values for the Worktree mode radio. Mirrors the
 * legacy `MeshPropertiesPanel` options verbatim. `branched` is the
 * default per `DEFAULT_WORKTREE_MODE` above.
 */
export type WorktreeModeForm = 'branched' | 'detached';

const WORKTREE_MODE_OPTIONS: { value: WorktreeModeForm; label: string }[] = [
  { value: 'branched', label: 'Branched — actual git branch per worktree (default)' },
  { value: 'detached', label: 'Detached — detached HEAD worktree' },
];

// Mappers between the form's two-option enum and the open-ended
// string values stored in `meshes.base_ref` / `meshes.worktree_mode`.
// We intentionally collapse anything other than the canonical values
// to the form's default — matches the legacy panel's
// `config.base_ref === 'HEAD' ? 'head' : 'fresh'` rule and the
// `config.worktree_mode ?? DEFAULT_WORKTREE_MODE` fallback.
export const wireToFormBaseRef = (wire: string | null): BaseRefForm =>
  wire === 'HEAD' ? 'head' : 'fresh';
export const wireToFormMode = (wire: string | null): WorktreeModeForm =>
  wire === 'detached' ? 'detached' : DEFAULT_WORKTREE_MODE;
export const formToWireBaseRef = (form: BaseRefForm): 'origin/main' | 'HEAD' =>
  form === 'head' ? 'HEAD' : 'origin/main';

export interface WorktreeStrategySectionProps {
  useWorktree: boolean;
  baseRef: BaseRefForm;
  worktreeMode: WorktreeModeForm;
  /** Per-mesh pre-spawn pool target (`0` = off, `1..=5`). The toggle's
   *  `checked` is derived from `preSpawnPoolSize > 0`. */
  preSpawnPoolSize: number;
  /**
   * Live count of `available` warm pool entries for the mesh. Drives
   * the `<PoolStatus>` row under the "Pre-spawn warm worktrees"
   * header. `null` = first fetch in flight (badge shows "…" instead
   * of a misleading "0/N").
   */
  poolCount: number | null;
  /** Per-Mesh worktree directory override draft (`''` = inherit, issue #1519). */
  worktreeDirectory: string;
  /** Inherited effective container dir for display (backend-authoritative). */
  worktreeDirEffective: string;
  /** Inline save failure, scoped to these controls. Kept separate from the
   *  destination's top-level `SaveIndicator` because a form this long needs
   *  the message to say WHICH control failed ("Failed to update base_ref"),
   *  and because the "form mirrors user intent" rule means the field keeps
   *  the user's typed value while this explains why it did not stick. */
  error?: string | null;
  onToggleUseWorktree: (next: boolean) => void;
  onChangeBaseRef: (next: BaseRefForm) => void;
  onChangeWorktreeMode: (next: WorktreeModeForm) => void;
  onChangePoolSize: (next: number) => void;
  onChangeWorktreeDirectory: (next: string) => void;
}

/**
 * The worktree strategy body. The section frame (heading, description,
 * spacing) belongs to the caller — Project Settings renders it inside a
 * `ProbeSection` labelled "Worktree strategy" so the destination's section
 * list stays in one readable column; this component only owns the four
 * controls and their internal directory draft.
 */
export function WorktreeStrategySection({
  useWorktree,
  baseRef,
  worktreeMode,
  preSpawnPoolSize,
  poolCount,
  worktreeDirectory,
  worktreeDirEffective,
  error,
  onToggleUseWorktree,
  onChangeBaseRef,
  onChangeWorktreeMode,
  onChangePoolSize,
  onChangeWorktreeDirectory,
}: WorktreeStrategySectionProps) {
  // Issue #1519: local draft so typing doesn't fire a backend save +
  // pool rebuild per keystroke. Committed on blur / Enter; the parent
  // refreshes `worktreeDirectory` from the backend on success, which
  // re-syncs this draft via the effect below.
  const [dirDraft, setDirDraft] = useState(worktreeDirectory);
  useEffect(() => {
    setDirDraft(worktreeDirectory);
  }, [worktreeDirectory]);
  const poolEnabled = preSpawnPoolSize > 0;
  // Display value for the size number input. When the toggle is off,
  // show 1 (the spec's default — the user hasn't picked a size yet, but
  // we need a placeholder that the disabled input can render). When the
  // toggle is on, `preSpawnPoolSize` is `>= 1` by the poolEnabled guard,
  // so the value is already a valid 1..5. The `|| 1` short-circuits the
  // edge case where the IPC clamp rejects a write and the form keeps
  // the stale 0 (defensive; `poolEnabled` would already be false).
  const poolInputDisplay = preSpawnPoolSize || 1;
  return (
    <div className="space-y-3">
      {/* Use worktree checkbox — the gate. When unchecked, the radio
          groups + the pre-spawn pool block below collapse. */}
      <label className="flex items-center gap-2 text-xs text-text-primary cursor-pointer">
        <input
          type="checkbox"
          checked={useWorktree}
          onChange={(e) => onToggleUseWorktree(e.target.checked)}
          className="accent-accent-cyan"
        />
        <span>Use worktree</span>
      </label>

      {useWorktree && (
        <div className="pl-4 border-l border-border-subtle space-y-3">
          {/* Pre-spawn pool (issue #611). The toggle is derived from
              `preSpawnPoolSize > 0`; the size input is disabled when
              the toggle is off. Sits ABOVE Starting point / Worktree
              mode because it's a worktree-strategy decision — the user
              should pick the warm-pool size before deciding where the
              base ref sits. */}
          <div className="space-y-2">
            <label className="flex items-center gap-2 text-xs text-text-primary cursor-pointer">
              <input
                type="checkbox"
                checked={poolEnabled}
                onChange={(e) =>
                  // Toggle off → 0 (disables the pool). Toggle on → 1
                  // (the spec's default; the size input then lets the
                  // user pick 2..5). The save fires immediately so the
                  // pool worker's next reconcile sees the new target.
                  onChangePoolSize(e.target.checked ? 1 : 0)
                }
                className="accent-accent-cyan"
              />
              <span>Pre-spawn warm worktrees</span>
            </label>
            {/* Live pool-ready badge. Hidden when the pool is disabled
                (`preSpawnPoolSize === 0`); otherwise shows the ratio
                `poolCount / target` plus a thin progress bar that fills
                proportionally. Listens to `pool-count-changed` events
                upstream (via `usePoolChanged` in the parent) so a
                successful spawn drops the bar in real time and the
                background worker's refill climbs it back up. A11y: the
                wrapper is `role="status"` + `aria-live="polite"` so
                screen readers announce changes, and the full
                human-readable label sits in `aria-label` / `title`. */}
            {poolEnabled && <PoolStatus count={poolCount} target={preSpawnPoolSize} />}
            <label
              className={`flex items-center gap-2 text-xs pl-4 ${
                poolEnabled
                  ? 'text-text-primary cursor-pointer'
                  : 'text-text-muted cursor-not-allowed'
              }`}
            >
              <span>Pool size</span>
              <input
                type="number"
                min={1}
                max={5}
                step={1}
                value={poolInputDisplay}
                disabled={!poolEnabled}
                onChange={(e) => {
                  // Clamp at the IPC boundary AND the input handler so
                  // an out-of-range typed value doesn't bounce through
                  // a save round-trip just to get rejected.
                  const n = Number(e.target.value);
                  if (Number.isFinite(n) && n >= 1 && n <= 5) {
                    onChangePoolSize(Math.trunc(n));
                  }
                }}
                className="w-12 px-1 py-0.5 rounded-md border border-border-subtle bg-bg-overlay text-text-primary disabled:opacity-50 disabled:cursor-not-allowed"
                title={
                  poolEnabled
                    ? '1 = one warm worktree pre-cut; 5 = five'
                    : 'Toggle "Pre-spawn warm worktrees" to set a size'
                }
              />
            </label>
            <p className="text-2xs text-text-muted pl-4">
              Pre-warm worktree directories at startup. Manual spawns
              land on a pre-cut directory in &lt;500ms instead of ~11s.
            </p>
          </div>

          {/* Starting point — Fresh / Head */}
          <fieldset className="border-0 p-0 m-0 space-y-2">
            <legend className="block text-xs text-text-muted mb-1">
              Starting point
            </legend>
            {BASEREF_OPTIONS.map((o) => (
              <label
                key={o.value}
                className="flex items-start gap-2 text-xs text-text-primary cursor-pointer"
              >
                <input
                  type="radio"
                  name="wt-cfg-baseref"
                  value={o.value}
                  checked={baseRef === o.value}
                  onChange={() => onChangeBaseRef(o.value)}
                  className="mt-0.5 accent-accent-cyan"
                />
                <span>{o.label}</span>
              </label>
            ))}
          </fieldset>

          {/* Worktree mode — Branched / Detached */}
          <fieldset className="border-0 p-0 m-0 space-y-2">
            <legend className="block text-xs text-text-muted mb-1">
              Worktree mode
            </legend>
            {WORKTREE_MODE_OPTIONS.map((o) => (
              <label
                key={o.value}
                className="flex items-start gap-2 text-xs text-text-primary cursor-pointer"
              >
                <input
                  type="radio"
                  name="wt-cfg-mode"
                  value={o.value}
                  checked={worktreeMode === o.value}
                  onChange={() => onChangeWorktreeMode(o.value)}
                  className="mt-0.5 accent-accent-cyan"
                />
                <span>{o.label}</span>
              </label>
            ))}
          </fieldset>

          {/* Worktree directory override (issue #1519). Empty = inherit the
              app default (or `.claude/worktrees`); relative resolves from the
              mesh root, absolute must match the mesh environment. Changing it
              affects future nodes + pool entries only — live nodes keep their
              persisted directories. */}
          <div className="space-y-1">
            <label className="flex flex-col gap-1 text-xs text-text-primary">
              <span>Worktree directory</span>
              <input
                type="text"
                aria-label="Worktree directory"
                placeholder=".claude/worktrees"
                value={dirDraft}
                onChange={(e) => setDirDraft(e.target.value)}
                onBlur={() => {
                  if (dirDraft !== worktreeDirectory) onChangeWorktreeDirectory(dirDraft);
                }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
                  if (e.key === 'Escape') setDirDraft(worktreeDirectory);
                }}
                className="w-full px-2 py-1 rounded-md border border-border-subtle bg-bg-overlay text-text-primary"
              />
            </label>
            <p className="text-2xs text-text-muted" aria-live="polite">
              Effective: <code>{worktreeDirEffective || '…'}</code>
              {worktreeDirectory.trim() === '' ? ' (inherited)' : ''}
            </p>
            {worktreeDirectory.trim() !== '' && (
              <button
                type="button"
                onClick={() => onChangeWorktreeDirectory('')}
                className="text-2xs text-text-muted underline hover:text-text-primary"
              >
                Reset to inherited
              </button>
            )}
          </div>
        </div>
      )}

      {/* Config-save failures stay inside this section: the indicator names
          the column that refused the write, which the destination-level
          `SaveIndicator` above the sections cannot do. */}
      {error && (
        <p className="text-xs text-status-error break-words" data-testid="worktree-strategy-error">
          {error}
        </p>
      )}
    </div>
  );
}

// ── Pool status badge ──────────────────────────────────────────────────────

/**
 * Live ready-vs-target indicator for the pre-spawn pool.
 *
 * **Bar (not text):** Project Settings is dense with controls — a 4px bar
 * is glanceable at a distance where a `${count}/${target}` label competes
 * with the size input next to it.
 *
 * **Muted by default (no green):** `refreshing` rows aren't counted as
 * ready, so a pool "at target" can still be mid-refresh; and `0 ready`
 * is the *expected* state during a spawn's in-flight claim. A green
 * "ready" colour would promise health that the count doesn't deliver.
 *
 * **A11y:** `role="status"` + `aria-live="polite"` so screen readers
 * announce updates; `aria-label` carries the full sentence because
 * the visible "…" placeholder is ambiguous.
 */
function PoolStatus({ count, target }: { count: number | null; target: number }) {
  const display = count === null ? '…' : String(count);
  const fullLabel = count === null
    ? 'Pool status unknown'
    : `${count} of ${target} pre-spawn worktrees ready`;
  // Clamp to [0, 100] — a transient over-full (count > target) is
  // capped rather than rendered as a >100% bar.
  const pct = count === null ? 0 : Math.max(0, Math.min(100, (count / Math.max(target, 1)) * 100));

  return (
    <div
      role="status"
      aria-live="polite"
      aria-label={fullLabel}
      title={fullLabel}
      className="flex items-center gap-2 pl-4 text-2xs text-text-muted"
      data-testid="pool-status"
    >
      <div
        className="relative h-1 flex-1 max-w-[120px] rounded-full bg-bg-overlay overflow-hidden"
        aria-hidden
      >
        <div
          className="absolute inset-y-0 left-0 bg-accent-cyan/70 transition-[width] duration-300"
          style={{ width: `${pct}%` }}
        />
      </div>
      <span className="tabular-nums" data-testid="pool-status-text">
        {display} / {target} ready
      </span>
    </div>
  );
}
