/**
 * `PromptsPane` — the initial-prompt templates Buildmesh sends to an agent it
 * spawns from an issue or a pull request. Its own tab because the templates
 * are long, multi-line editors that people touch rarely and that would
 * otherwise push everyday settings off the General page.
 *
 * `null` for a kind means "no override, the built-in default is active". The
 * defaults are static per binary with no mesh context, so they load once
 * outside the resource state machine; the editors stay disabled until they
 * resolve. A failure surfaces inline (with Retry) rather than leaving the
 * cards silently disabled.
 */
import { useCallback, useEffect, useState } from 'react';
import * as api from '../../lib/tauri';
import type { AppPreferences } from '../../types/generated/AppPreferences';
import { formatError } from '../../lib/errorUtils';
import { ResourceLoadStatus } from './ResourceLoadStatus';
import { ProbeSpawnPromptsSection, type ProbePromptDefaults, type ProbePromptKind } from './ProbeSpawnPromptsSection';
import { useSettingsData } from './SettingsDataContext';

export function PromptsPane() {
  const { resources, prefsLoaded, preferences, loadPreferences, retryResource, setError, siteDirtyChange } =
    useSettingsData();

  const [probePrompts, setProbePrompts] = useState<{ issue: string | null; pr: string | null }>({
    issue: null,
    pr: null,
  });
  const [probePromptDefaults, setProbePromptDefaults] = useState<ProbePromptDefaults | null>(null);
  const [probeDefaultsError, setProbeDefaultsError] = useState<string | null>(null);
  // The preferences payload whose prompts are currently rendered.
  const [adoptedPrefs, setAdoptedPrefs] = useState<AppPreferences | null>(null);

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

  // Seed from the latest winning preferences read during render (the same
  // adjust-state-while-rendering pattern the other panes use), so the editors
  // are never enabled while still showing the pre-load value.
  if (preferences && preferences !== adoptedPrefs) {
    setAdoptedPrefs(preferences);
    setProbePrompts({
      issue: preferences.issue_spawn_prompt ?? null,
      pr: preferences.pr_spawn_prompt ?? null,
    });
  }

  const probePromptsDirtyChange = useCallback(
    (dirty: boolean) => siteDirtyChange('probe-prompts', dirty),
    [siteDirtyChange],
  );

  // One save path per side; the boolean return drives the section's optimistic
  // commit/rollback, and the post-write `loadPreferences()` picks up the
  // backend normalise-on-write (a blank draft clears the override) so the
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
      {resources.preferences.status === 'failed' && (
        <ResourceLoadStatus
          resource="preferences"
          state={resources.preferences}
          onRetry={() => retryResource('preferences')}
        />
      )}
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
    </>
  );
}
