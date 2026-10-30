/**
 * `GeneralPane` — appearance, quit confirmation and the default worktree
 * directory (issue #1880). Owns every draft the General tab edits: the theme
 * choice, the confirm-before-quit toggle, and the Worktree Node directory.
 * Circuit limits live on the Autopilot tab and the probe spawn prompts on the
 * Prompts tab.
 *
 * The worktree directory hydrates from the shared `preferences` payload and
 * commits on blur / Enter rather than per keystroke, so a half-typed path
 * never briefly relocates the warm pool. It reports a dirty site so the
 * modal's nav rail can mark this pane and the discard banner can intercept a
 * stray close (issue #730).
 *
 * The blur commit is guarded against the modal's discard prompt
 * (issue #2458): Escape, the header × and a backdrop click all move focus
 * off the field, and an unguarded blur-commit would persist the very draft
 * the user is trying to abandon.
 */
import { useEffect, useRef, useState } from 'react';
import * as api from '../../lib/tauri';
import type { AppPreferences } from '../../types/generated/AppPreferences';
import { formatError } from '../../lib/errorUtils';
import { currentTheme, setTheme, type ThemeName } from '../../lib/theme';
import {
  TEXT_SCALE_DEFAULT,
  TEXT_SCALE_MAX,
  TEXT_SCALE_MIN,
  TEXT_SCALE_STEP,
  setTextScale,
} from '../../lib/textScale';
import { useTextScale } from '../../hooks/useTextScale';
import { useExitPromptStore } from '../../stores/exitPromptStore';
import { optimisticToggle } from '../../lib/optimisticToggle';
import { useBlurCommit } from '../../hooks/useBlurCommit';
import { SettingsRow, SettingsSection } from './SettingsRow';
import { ResourceLoadStatus } from './ResourceLoadStatus';
import { UpdateAboutSection } from './UpdateAboutSection';
import { CompanionSettingsSection } from './CompanionSettingsSection';
import { useSettingsData } from './SettingsDataContext';

/**
 * Interface text size — the accessibility size slider for everything the app
 * draws outside a terminal.
 *
 * No new store: `textScale.ts` owns the value and writes `--ui-text-scale` on
 * <html>, where every `text-*` step in App.css multiplies by it. The slider is
 * a view of that one number — the same arrangement the title bar's terminal
 * Zoom control has with `terminalConfig`, so the two sliders behave
 * identically to the user.
 *
 * Commits per input event rather than on release: a text-size slider is
 * useless if the text does not resize while you drag it. Like the theme radio, it
 * is not a dirty draft — the write is synchronous localStorage plus a custom
 * property, so there is no half-saved state for a "Discard changes?" prompt to
 * protect.
 */
function InterfaceTextSizeControl() {
  // One subscription to the store the whole app reads, so the slider and its
  // readout track the value wherever it changes from.
  const percent = Math.round(useTextScale() * 100);

  const commit = (nextPercent: number) => setTextScale(nextPercent / 100);
  const defaultPercent = TEXT_SCALE_DEFAULT * 100;

  return (
    <>
      <div className="mb-2 flex items-center justify-between">
        <span className="text-xs text-text-secondary">Text size</span>
        <span
          className="font-mono text-xs text-text-primary"
          data-testid="interface-text-scale-value"
        >
          {percent}%
        </span>
      </div>
      <input
        type="range"
        min={TEXT_SCALE_MIN * 100}
        max={TEXT_SCALE_MAX * 100}
        step={Math.round(TEXT_SCALE_STEP * 100)}
        value={percent}
        onChange={(e) => commit(Number(e.target.value))}
        aria-label="Interface text size"
        // The slider's own value is a bare number; the readout says percent.
        aria-valuetext={`${percent}%`}
        data-testid="interface-text-scale-slider"
        className="w-full cursor-pointer accent-accent-cyan"
      />
      <div className="mt-2 flex items-center justify-between">
        {/* Decorative size swatches — the slider carries the value for
            assistive tech, so these are hidden from the a11y tree. */}
        <span aria-hidden="true" className="text-2xs leading-none text-text-muted">A</span>
        <button
          type="button"
          onClick={() => commit(defaultPercent)}
          disabled={percent === defaultPercent}
          data-testid="interface-text-scale-reset"
          className="rounded-md px-1 text-sm text-text-muted transition-colors hover:text-accent-cyan disabled:cursor-default disabled:text-text-muted disabled:hover:text-text-muted"
        >
          Reset
        </button>
        <span aria-hidden="true" className="text-base leading-none text-text-muted">A</span>
      </div>
    </>
  );
}

export function GeneralPane() {
  const {
    resources,
    prefsLoaded,
    preferences,
    loadPreferences,
    retryResource,
    setError,
    siteDirtyChange,
  } = useSettingsData();

  // Issue #2458 — the blur commit is skipped while the modal is intercepting
  // a dismissal, so an abandoned draft stays uncommitted and the discard
  // prompt can still ask about it.
  const commitOnBlur = useBlurCommit();

  // Issue #734: theme toggle. `themeDraft` mirrors currentTheme() so the
  // radio reflects the active value on modal open; flipping it calls
  // setTheme(), which writes localStorage, updates <html data-theme>,
  // AND fires the module-level pub/sub that every ThemeManager listens
  // to — so agent terminals and build-run terminals flip in lockstep
  // without this pane touching either registry directly. No rollback
  // path — setTheme is synchronous and writes to a synchronous
  // localStorage key, so a "failed save" isn't possible.
  const [themeDraft, setThemeDraft] = useState<ThemeName>(currentTheme);

  // Issue #1501: confirm-before-quit. The value lives in
  // `useExitPromptStore` (not local state) so the window-close guard reads
  // the same synchronous source of truth the checkbox writes — no cold IPC
  // on the close path. `true` is the default (fresh install or older
  // preferences.json without the field).
  const confirmBeforeQuit = useExitPromptStore((s) => s.confirmBeforeQuit);
  const [confirmQuitBusy, setConfirmQuitBusy] = useState(false);

  // The preferences payload whose drafts are currently rendered. Null until
  // the first load commits.
  const [adoptedPrefs, setAdoptedPrefs] = useState<AppPreferences | null>(null);

  // Issue #1519: Buildmesh-wide default Worktree Node directory. Draft is
  // the raw textfield value; `''` means "no app override — each Mesh uses
  // `.worktrees` unless it has its own override".
  const [worktreeDirDraft, setWorktreeDirDraft] = useState('');
  const [worktreeDirSaving, setWorktreeDirSaving] = useState(false);
  const worktreeDirSavedRef = useRef('');

  // Seed the draft from the latest winning preferences read *during render*
  // rather than from an effect. The input is controlled by the draft and its
  // `disabled` gate flips in the same render as the resource status, so
  // an effect would leave one committed frame where the input is enabled and
  // still showing the previous value. Each payload is a distinct object (the
  // hook only commits a read whose token is still current), so identity is a
  // sufficient "this load landed" signal — and re-seeding on a post-save
  // refresh is what picks up a backend normalise-on-write.
  if (preferences && preferences !== adoptedPrefs) {
    setAdoptedPrefs(preferences);
    setWorktreeDirDraft(preferences.worktree_directory?.trim() ?? '');
  }

  // Last value confirmed saved (canonical string form), for rollback and the
  // dirty comparison. A ref for the closure-staleness reason documented on the
  // optimistically-rolled-back pickers (issue #581): two commits fired in quick
  // succession must each roll back to the value as of their own selection, not
  // to one shared snapshot.
  //
  // Seeded from an EFFECT rather than written inside the render-phase adoption
  // above. React's adjust-state-while-rendering pattern licenses `setState` for
  // the rendering component; writing a ref during render is impure, and a
  // double-invoked render writes twice. The effect runs before any user
  // interaction can blur or type, so the values are current by the time a
  // commit reads them.
  useEffect(() => {
    if (!preferences) return;
    worktreeDirSavedRef.current = preferences.worktree_directory?.trim() ?? '';
  }, [preferences]);

  // Syncing `useExitPromptStore` cannot happen in the render-phase block above:
  // that store is external and `App.tsx` / `WindowCloseGuard` subscribe to it,
  // so writing during render would update a *different* component mid-render
  // (React warns) and would run twice under StrictMode. The effect restores
  // the post-commit timing the loader callback used to have, and keeps the
  // checkbox and the window-close guard on one synchronous source of truth
  // (issue #1501).
  useEffect(() => {
    if (!preferences) return;
    useExitPromptStore.getState().setConfirmBeforeQuit(preferences.confirm_before_quit ?? true);
  }, [preferences]);

  // Commit the worktree-directory draft (blur / Enter, issue #1519). `''`
  // (or whitespace-only) clears the app default so inheriting Meshes fall
  // back to `.worktrees`; anything else stores the trimmed raw input
  // verbatim (no shell/`~` expansion — resolution joins it literally).
  // Optimistic with rollback.
  const commitWorktreeDir = async () => {
    const trimmed = worktreeDirDraft.trim();
    const canonical = trimmed;
    setWorktreeDirDraft(canonical);
    if (canonical === worktreeDirSavedRef.current) {
      siteDirtyChange('worktree-dir', false);
      return;
    }
    const previous = worktreeDirSavedRef.current;
    worktreeDirSavedRef.current = canonical;
    siteDirtyChange('worktree-dir', false);
    setWorktreeDirSaving(true);
    setError(null);
    try {
      await api.setAppWorktreeDirectory(canonical === '' ? null : canonical);
      // Issue #1534 (review round 4) — route the post-write
      // preferences refresh through the loader so a rejection
      // surfaces in the preferences banner rather than leaving
      // the optimistic draft silently stale.
      await loadPreferences();
    } catch (e) {
      worktreeDirSavedRef.current = previous;
      setWorktreeDirDraft(previous);
      setError(formatError(e));
    } finally {
      setWorktreeDirSaving(false);
    }
  };

  // Issue #1501: flip the exit-confirmation prompt. Optimistic with
  // rollback so the checkbox never lies about the persisted value. The
  // store is the single writer: the close guard's synchronous read sees
  // the new value immediately, even before the backend confirms.
  const handleToggleConfirmQuit = (enabled: boolean) =>
    optimisticToggle({
      current: useExitPromptStore.getState().confirmBeforeQuit,
      next: enabled,
      setValue: (v) => useExitPromptStore.getState().setConfirmBeforeQuit(v),
      setBusy: setConfirmQuitBusy,
      setError,
      mutation: async () => {
        await api.setAppConfirmBeforeQuit(enabled);
      },
      // Issue #1534 (review round 5) — refresh via the loader so a
      // backend rejection on the post-write preferences read surfaces
      // in the preferences banner. Best-effort: a refresh failure
      // must NOT roll back the successful mutation (the value is
      // already persisted).
      onSuccess: () => {
        loadPreferences().catch((err) => {
          console.warn('[AppSettings] Failed to refresh preferences after confirm-quit toggle:', err);
        });
      },
    });

  // Issue #734: persist the theme choice. `setTheme` is the single
  // entry point — it writes localStorage, sets/clears <html data-theme>,
  // and fires the module-level pub/sub that BOTH registries'
  // ThemeManager instances subscribe to. So one call here updates the
  // agent terminal AND the build/run terminal in lockstep. No rollback:
  //   localStorage writes are synchronous and the DOM/xterm flips are
  //   in-memory. The dirty-tracker is intentionally NOT involved — a
  // theme flip is an instant visual change with no half-saved state,
  // so a "Discard unsaved changes?" prompt would be more confusing
  // than helpful.
  const handleSaveTheme = (next: ThemeName) => {
    if (next === themeDraft) return;
    setThemeDraft(next);
    setTheme(next);
  };

  return (
    <>
      {/* Issue #1534 (review round 5) — surface a loading indicator while
          preferences / providers are still loading on modal open. Without
          this, the user opens the modal, every control is silently disabled,
          and there's no signal that the data hasn't arrived yet — looks
          identical to a frozen crash. `role="status"` (polite) so screen
          readers announce the transition without interrupting. */}
      {(resources.preferences.status === 'loading' ||
        resources.providers.status === 'loading') && (
        <p
          className="text-base text-text-muted"
          data-testid="resource-load-general-loading"
          role="status"
          aria-live="polite"
        >
          Loading settings…
        </p>
      )}
      {/* Issue #1534 — preference-backed controls in this pane (worktree
          directory, confirm-quit) stay disabled while preferences
          load or have failed; the banner makes that visible at the top of the
          pane instead of leaving the user to discover the disabled inputs by
          trial. */}
      {resources.preferences.status === 'failed' && (
        <ResourceLoadStatus
          resource="preferences"
          state={resources.preferences}
          onRetry={() => retryResource('preferences')}
        />
      )}

      {/* Appearance — per-machine colour theme. */}
      <SettingsSection title="Appearance">
        {/* Issue #2467 — no `htmlFor` here: `<fieldset>` is not a labelable
            element, so a `<label for="theme-radio-group">` resolving to it
            left `label.control === null` and an orphaned association. The
            fieldset carries `aria-label="Theme"` as its own accessible name. */}
        <SettingsRow
          label="Theme"
          summary="Colour theme for the app and terminals."
          layout="stacked"
          details={
            <>
              Dark is the default; light inverts the surface and text tokens while
              keeping the accent palette intact. The choice is saved per machine —
              xterm.js terminals flip with the rest of the app.
            </>
          }
        >
          <fieldset
            id="theme-radio-group"
            aria-label="Theme"
            className="flex flex-wrap gap-2"
          >
            {(['dark', 'light'] as const).map((name) => (
              <label
                key={name}
                className={`flex items-center gap-2 px-4 py-2 rounded-md text-base cursor-pointer border transition-colors ${
                  themeDraft === name
                    ? 'bg-bg-card border-accent-cyan text-text-primary'
                    : 'bg-bg-card border-border-subtle text-text-secondary hover:border-border-default'
                }`}
              >
                <input
                  type="radio"
                  name="theme"
                  value={name}
                  checked={themeDraft === name}
                  // Controlled radio: the picker is always in step with the
                  // active theme (setTheme is synchronous). Each click commits
                  // immediately — no "Save" button, no dirty site, no rollback.
                  onChange={() => handleSaveTheme(name)}
                  className="accent-accent-cyan"
                  data-testid={`theme-radio-${name}`}
                />
                <span className="capitalize">{name}</span>
              </label>
            ))}
          </fieldset>
        </SettingsRow>

        {/* Interface text size sits beside Theme because both are per-machine
            presentation choices with no backend half. */}
        <SettingsRow
          label="Interface text size"
          summary="Size of the app's text. Terminals keep their own Zoom size."
          layout="stacked"
          details={
            <>
              100% is the size Buildmesh ships with. Raising it enlarges text in
              the sidebar, the Probe, the title bar and every dialog at once — the
              layout, control sizes and terminals stay put. Terminals are sized
              separately by the title bar's Zoom control.
            </>
          }
        >
          <InterfaceTextSizeControl />
        </SettingsRow>
      </SettingsSection>

      {/* Issue #1501: exit confirmation. On by default — closing the
          window with active sessions prompts instead of terminating. The
          row label is the checkbox's accessible name; the explanation lives
          behind the info tip rather than beside the control. */}
      <SettingsSection title="Behaviour">
        <SettingsRow
          label="Confirm before quitting"
          htmlFor="confirm-before-quit"
          controlClassName="shrink-0"
          details="Warn before closing the window while agent sessions are active."
        >
          <input
            id="confirm-before-quit"
            type="checkbox"
            checked={confirmBeforeQuit}
            disabled={!prefsLoaded || confirmQuitBusy}
            onChange={e => void handleToggleConfirmQuit(e.target.checked)}
            className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
          />
        </SettingsRow>
      </SettingsSection>

      <SettingsSection title="Agent runtime">
        <SettingsRow
          label="Worktree directory"
          htmlFor="worktree-directory"
          summary="Default folder for new worktree nodes, relative to each mesh root."
          controlClassName="w-72 shrink-0"
          details={
            <>
              Default folder for new Worktree Nodes across{' '}
              <span className="font-medium">all</span> meshes. Relative paths
              resolve from each mesh root (e.g. <code>worktrees</code>); absolute
              paths are not allowed here because one default spans both native and
              WSL meshes — set an absolute path as a per-mesh override in Project
              Settings instead. Leave empty for <code>.worktrees</code>. A
              per-mesh override in Project Settings takes precedence. Changing this
              affects future nodes and pre-spawn pool entries only — live nodes
              keep their existing directories.
            </>
          }
        >
          <input
            id="worktree-directory"
            type="text"
            aria-label="Worktree directory"
            placeholder=".worktrees"
            value={worktreeDirDraft}
            disabled={!prefsLoaded || worktreeDirSaving}
            onChange={e => {
              setWorktreeDirDraft(e.target.value);
              siteDirtyChange('worktree-dir', e.target.value.trim() !== worktreeDirSavedRef.current);
            }}
            onBlur={commitOnBlur(commitWorktreeDir)}
            onKeyDown={e => {
              if (e.key === 'Enter') commitWorktreeDir();
            }}
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          />
        </SettingsRow>
      </SettingsSection>

      {/* Issue #1526 — manual update surface. The auto-launch prompt
          (UpdatePrompt) handles nag-style flow; Settings exposes
          current version + manual check + install progress for users
          who want to drive it themselves. */}
      <UpdateAboutSection />
      <CompanionSettingsSection />
    </>
  );
}
