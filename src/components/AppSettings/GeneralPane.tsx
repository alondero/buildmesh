/**
 * `GeneralPane` — appearance, behaviour and host-wide agent runtime
 * defaults (issue #1880). Owns every draft the General tab edits:
 * the theme choice, the confirm-before-quit toggle, the Circuit agent
 * pool size, the Worktree Node directory, and the probe spawn prompt
 * templates.
 *
 * All four preference-backed drafts hydrate from the shared
 * `preferences` payload and commit on blur / Enter rather than per
 * keystroke, so a half-typed "1" of "10" never briefly caps the pool at
 * 1, and a half-typed path never briefly relocates the warm pool. Each
 * draft also reports a dirty site so the modal's nav rail can mark this
 * pane and the discard banner can intercept a stray close (issue #730).
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import * as api from '../../lib/tauri';
import type { AppPreferences } from '../../types/generated/AppPreferences';
import { formatError } from '../../lib/errorUtils';
import { currentTheme, setTheme, type ThemeName } from '../../lib/theme';
import { useExitPromptStore } from '../../stores/exitPromptStore';
import { optimisticToggle } from '../../lib/optimisticToggle';
import { SettingsRow, SettingsSection } from './SettingsRow';
import { ResourceLoadStatus } from './ResourceLoadStatus';
import { ProbeSpawnPromptsSection, type ProbePromptDefaults, type ProbePromptKind } from './ProbeSpawnPromptsSection';
import { UpdateAboutSection } from './UpdateAboutSection';
import { useSettingsData } from './SettingsDataContext';

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

  // Circuit agent pool size (app-wide cap on concurrent Circuit agents).
  // The draft is a string so the input can hold a cleared/in-progress
  // value; `''` means "no global cap".
  const [poolDraft, setPoolDraft] = useState('');
  const [poolSaving, setPoolSaving] = useState(false);
  // The preferences payload whose drafts are currently rendered. Null until
  // the first load commits.
  const [adoptedPrefs, setAdoptedPrefs] = useState<AppPreferences | null>(null);
  // Last value confirmed saved (canonical string form), for rollback and
  // dirty comparison. A ref for the same closure-staleness reason as the
  // optimistically-rolled-back pickers (issue #581).
  const poolSavedRef = useRef('');

  // Issue #1519: Buildmesh-wide default Worktree Node directory. Draft is
  // the raw textfield value; `''` means "no app override — each Mesh uses
  // `.claude/worktrees` unless it has its own override".
  const [worktreeDirDraft, setWorktreeDirDraft] = useState('');
  const [worktreeDirSaving, setWorktreeDirSaving] = useState(false);
  const worktreeDirSavedRef = useRef('');

  // Probe spawn prompt templates. `null` = no override, the built-in
  // default is active.
  const [probePrompts, setProbePrompts] = useState<{ issue: string | null; pr: string | null }>({
    issue: null,
    pr: null,
  });
  const [probePromptDefaults, setProbePromptDefaults] = useState<ProbePromptDefaults | null>(null);

  // Built-in probe-spawn templates for the Settings display. Static per
  // binary with no mesh context, so they load once outside the resource
  // state machine; the section stays disabled until they resolve. A
  // failure surfaces inline in the section (with Retry) rather than
  // leaving the cards silently disabled.
  const [probeDefaultsError, setProbeDefaultsError] = useState<string | null>(null);
  const loadProbeDefaults = useCallback(() => {
    setProbeDefaultsError(null);
    return api
      .getProbeSpawnPromptDefaults()
      .then((d) => {
        setProbePromptDefaults({ issue: d.issue_template, pr: d.pr_template, policy: d.review_policy });
      })
      .catch((err) => {
        console.error('Failed to load probe prompt defaults:', err);
        setProbeDefaultsError(formatError(err));
      });
  }, []);
  useEffect(() => {
    void loadProbeDefaults();
  }, [loadProbeDefaults]);

  // Seed every draft from the latest winning preferences read *during render*
  // rather than from an effect. These inputs are controlled by the drafts and
  // their `disabled` gate flips in the same render as the resource status, so
  // an effect would leave one committed frame where the input is enabled and
  // still showing the previous value. Each payload is a distinct object (the
  // hook only commits a read whose token is still current), so identity is a
  // sufficient "this load landed" signal — and re-seeding on a post-save
  // refresh is what picks up a backend normalise-on-write.
  if (preferences && preferences !== adoptedPrefs) {
    setAdoptedPrefs(preferences);
    setPoolDraft(preferences.circuit_agent_pool_size == null ? '' : String(preferences.circuit_agent_pool_size));
    setWorktreeDirDraft(preferences.worktree_directory?.trim() ?? '');
    setProbePrompts({
      issue: preferences.issue_spawn_prompt ?? null,
      pr: preferences.pr_spawn_prompt ?? null,
    });
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
    poolSavedRef.current =
      preferences.circuit_agent_pool_size == null ? '' : String(preferences.circuit_agent_pool_size);
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

  const poolDirtyChange = useCallback(
    (dirty: boolean) => siteDirtyChange('circuit-agent-pool', dirty),
    [siteDirtyChange],
  );
  const probePromptsDirtyChange = useCallback(
    (dirty: boolean) => siteDirtyChange('probe-prompts', dirty),
    [siteDirtyChange],
  );

  // Commit the worktree-directory draft (blur / Enter, issue #1519). `''`
  // (or whitespace-only) clears the app default so inheriting Meshes fall
  // back to `.claude/worktrees`; anything else stores the trimmed raw input
  // verbatim (no shell/`~` expansion — resolution joins it literally).
  // Optimistic with rollback, mirroring the pool-size write.
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

  // Probe spawn prompt templates. One save path per side; the boolean
  // return drives the section's optimistic commit/rollback, and the
  // post-write `loadPreferences()` picks up the backend
  // normalise-on-write (a blank draft clears the override) so the
  // "Using default" badge flips without a stale snapshot.
  const handleSetProbePrompt = async (kind: ProbePromptKind, value: string): Promise<boolean> => {
    setError(null);
    try {
      if (kind === 'issue') await api.setAppIssueSpawnPrompt(value);
      else await api.setAppPrSpawnPrompt(value);
      await loadPreferences();
      return true;
    } catch (e) {
      setError(formatError(e));
      return false;
    }
  };

  const handleResetProbePrompt = async (kind: ProbePromptKind): Promise<boolean> => {
    setError(null);
    try {
      if (kind === 'issue') await api.setAppIssueSpawnPrompt(null);
      else await api.setAppPrSpawnPrompt(null);
      await loadPreferences();
      return true;
    } catch (e) {
      setError(formatError(e));
      return false;
    }
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
      {/* Issue #1534 — preference-backed controls in this pane (pool size,
          worktree directory, confirm-quit) stay disabled while preferences
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
        <SettingsRow
          label="Theme"
          htmlFor="theme-radio-group"
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
      </SettingsSection>

      {/* Issue #1501: exit confirmation. On by default — closing the
          window with active sessions prompts instead of terminating. The
          checkbox keeps its full label text because that string is the
          control's accessible name (used by tests and screen readers). */}
      <SettingsSection title="Behaviour">
        <SettingsRow
          label="Exiting"
          summary="Warn before closing the window with active agent sessions."
          controlClassName="w-80 shrink-0"
        >
          <label className="flex items-center gap-3 text-base text-text-primary cursor-pointer">
            <input
              type="checkbox"
              checked={confirmBeforeQuit}
              disabled={!prefsLoaded || confirmQuitBusy}
              onChange={e => void handleToggleConfirmQuit(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
            />
            <span>Confirm before quitting when agent sessions are active</span>
          </label>
        </SettingsRow>
      </SettingsSection>

      {/* Agent runtime — host-wide execution caps and paths. */}
      <SettingsSection
        title="Agent runtime"
        description={
          <>
            Host-wide execution defaults. Each mesh still respects its own
            concurrency limit and worktree override in Project Settings.
          </>
        }
      >
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
              Settings instead. Leave empty for <code>.claude/worktrees</code>. A
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
            placeholder=".claude/worktrees"
            value={worktreeDirDraft}
            disabled={!prefsLoaded || worktreeDirSaving}
            onChange={e => {
              setWorktreeDirDraft(e.target.value);
              siteDirtyChange('worktree-dir', e.target.value.trim() !== worktreeDirSavedRef.current);
            }}
            onBlur={commitWorktreeDir}
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
      <ProbeSpawnPromptsSection
        stored={probePrompts}
        defaults={probePromptDefaults}
        onSave={handleSetProbePrompt}
        onReset={handleResetProbePrompt}
        onDirtyChange={probePromptsDirtyChange}
        defaultsError={probeDefaultsError}
        onRetryDefaults={() => void loadProbeDefaults()}
        disabled={!prefsLoaded}
      />

      <UpdateAboutSection />
    </>
  );
}