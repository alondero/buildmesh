import { _invoke } from './_invoke';
import { emit } from '@tauri-apps/api/event';
import { clearDefaultProviderPromises } from '../providerCache';
import { PROVIDER_LIST_CHANGED_EVENT } from './provider';
import type { AppPreferences } from '../../types/generated/AppPreferences';
import type { CorruptionInfo } from '../../types/generated/CorruptionInfo';
import type { CorruptionReason } from '../../types/generated/CorruptionReason';
import type { PreferencesHealth } from '../../types/generated/PreferencesHealth';
import type { PreferencesStatus } from '../../types/generated/PreferencesStatus';
import type { RecoveryOutcome } from '../../types/generated/RecoveryOutcome';
import type { ProbeSpawnPromptDefaults } from '../../types/generated/ProbeSpawnPromptDefaults';

// ── App-wide preferences (`preferences.json`) ──────────────────────────────
//
// Generated from `crate::preferences::AppPreferences` (issue #404). The
// `google_cloud_project` field is included to match the Rust struct in full
// even though the current settings UI only reads two fields.
export type { AppPreferences };

export type { CorruptionInfo, CorruptionReason, PreferencesHealth, PreferencesStatus, RecoveryOutcome };

export const getAppPreferences = () =>
  _invoke<AppPreferences>('get_app_preferences');

// ── Corrupt-file recovery (issue #1523) ───────────────────────────────────
//
// `get_app_preferences` still returns defaults for an unreadable file so
// every read-only surface keeps working; `get_preferences_health` is the
// separate, *successful* call that says whether those defaults are real
// settings or a stand-in. Keeping it a distinct command is deliberate —
// folding corruption into an `Err` would force the UI to classify the
// failure by matching error prose.

export const getPreferencesHealth = () =>
  _invoke<PreferencesHealth>('get_preferences_health');

/** Put the last-known-good backup back over a corrupt file, archiving
 *  whatever is on disk first. The non-destructive recovery. */
export const restorePreferencesBackup = () =>
  _invoke<RecoveryOutcome>('restore_preferences_backup');

/** Archive the current file and start from defaults. Destructive by design:
 *  the caller must confirm it, and the returned `archive_path` says where the
 *  original bytes went. */
export const resetAppPreferences = () =>
  _invoke<RecoveryOutcome>('reset_app_preferences');

/** Open the folder holding `preferences.json` in the OS file manager. The
 *  directory, not the file — a file manager hands a `.json` to whatever
 *  application claims the extension, which launches an editor instead of
 *  showing the folder. */
export const openPreferencesLocation = () =>
  _invoke('open_preferences_location');

/** Pass `null` (or an empty string, which the backend filters out) to clear
 *  the override and fall back to the hardcoded `anthropic` default. */
export const setAppDefaultProvider = (provider: string | null) =>
  _invoke('set_app_default_provider', { provider }).then((result) => {
    // The app-wide default feeds every mesh's resolution — evict all cached
    // defaults and notify open surfaces, mirroring updateMeshColumn above.
    clearDefaultProviderPromises();
    void emit(PROVIDER_LIST_CHANGED_EVENT).catch(() => {});
    return result;
  });

/** App-wide reviewer Spawn Option. `null` restores the source-agent fallback;
 * this is separate from the ordinary default provider so adversarial reviews
 * can use an independent harness. */
export const setAppReviewerProvider = (provider: string | null) =>
  _invoke('set_app_reviewer_provider', { provider });

/** Issue #824: pick the backend that summarises PTY output into a slug.
 *  Pass `null` (or empty) to **disable** auto-naming — nodes keep their
 *  random `adjective-adjective-noun` slugs until the user picks a value
 *  in Settings → Auto-naming. Distinct from `default_provider`: a
 *  rename runs frequently on trivial content, so the user opts in
 *  explicitly rather than inheriting whatever expensive tier the
 *  spawned node happens to be on. */
export const setAppNamingProvider = (provider: string | null) =>
  _invoke('set_app_naming_provider', { provider });

/** App-wide Circuit agent pool cap (`null` = uncapped, `0` = pause new spawns).
 *  Semantics documented on `AppPreferences::circuit_agent_pool_size`. */
export const setAppCircuitAgentPoolSize = (size: number | null) =>
  _invoke('set_app_circuit_agent_pool_size', { size });

/** Whether to confirm before quitting with active agent sessions (issue #1501).
 *  `true` (default) surfaces the exit-confirmation modal; `false` closes
 *  without friction. */
export const setAppConfirmBeforeQuit = (confirm: boolean) =>
  _invoke('set_app_confirm_before_quit', { confirm });

/** Buildmesh-wide default Worktree Node directory (issue #1519).
 *  Pass `null` (or blank) to clear and restore `.worktrees` under
 *  each inheriting Mesh root. Relative resolves from each Mesh root;
 *  absolute stored verbatim (per-Mesh env validation at resolution).
 *  Triggers a background rebuild of inheriting pools; live nodes keep
 *  their persisted `worktree_path`. */
export const setAppWorktreeDirectory = (directory: string | null) =>
  _invoke('set_app_worktree_directory', { directory });

/** Built-in probe-spawn prompt templates (placeholders unrendered),
 *  shown in Settings as the defaults. Generated from
 *  commands::preferences::ProbeSpawnPromptDefaults.*/
export type { ProbeSpawnPromptDefaults };

export const getProbeSpawnPromptDefaults = () =>
  _invoke<ProbeSpawnPromptDefaults>('get_probe_spawn_prompt_defaults');

/** Custom template for the initial prompt of agents spawned from the
 *  Probe's GitHub Issues tab. Pass `null` (or blank, which the backend
 *  collapses) to clear the override and restore the built-in wording.*/
export const setAppIssueSpawnPrompt = (prompt: string | null) =>
  _invoke('set_app_issue_spawn_prompt', { prompt });

/** Custom template for the initial prompt of agents spawned from the
 *  Probe's Pull Requests tab. Pass `null` (or blank) to clear and
 *  restore the built-in wording.*/
export const setAppPrSpawnPrompt = (prompt: string | null) =>
  _invoke('set_app_pr_spawn_prompt', { prompt });
