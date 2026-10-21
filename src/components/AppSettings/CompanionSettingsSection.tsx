import { useState } from 'react';
import { saveCompanionSettings } from '../../lib/tauri/companion';
import { formatError } from '../../lib/errorUtils';
import { SettingsRow, SettingsSection } from './SettingsRow';
import { useSettingsData } from './SettingsDataContext';
import type { CompanionSettings } from '../../types/generated/CompanionSettings';

export function CompanionSettingsSection() {
  const { preferences, prefsLoaded, loadPreferences, setError } = useSettingsData();
  const [busy, setBusy] = useState(false);
  const settings = preferences?.companion ?? { launch_configuration: null, spawn_limit: 5 };
  const save = async (next: CompanionSettings) => {
    setBusy(true);
    try { await saveCompanionSettings(next); await loadPreferences(); }
    catch (error) { setError(formatError(error)); }
    finally { setBusy(false); }
  };
  return <SettingsSection title="Companion AI">
    <SettingsRow label="Launch Configuration" htmlFor="companion-launch" summary="Uses your Claude subscription login. Native host configurations only; API routes are rejected.">
      <select id="companion-launch" disabled={!prefsLoaded || busy} value={settings.launch_configuration ?? ''}
        className="w-full rounded-md border border-border-default bg-bg-card p-2 text-text-primary"
        onChange={event => void save({ ...settings, launch_configuration: event.target.value || null })}>
        <option value="">App default (Claude when unset)</option>
        <option value="claude">Claude Code (native defaults)</option>
        {preferences?.spawn_configurations?.map(config => <option key={config.id} value={config.id}>{config.name}</option>)}
      </select>
    </SettingsRow>
    <SettingsRow label="Maximum nodes per task" htmlFor="companion-limit" summary="Default 5. Hard limit 10, shared across every spawn in a task.">
      <select id="companion-limit" disabled={!prefsLoaded || busy} value={settings.spawn_limit}
        className="rounded-md border border-border-default bg-bg-card p-2 text-text-primary"
        onChange={event => void save({ ...settings, spawn_limit: Number(event.target.value) })}>
        {Array.from({ length: 10 }, (_, index) => index + 1).map(limit => <option key={limit}>{limit}</option>)}
      </select>
    </SettingsRow>
  </SettingsSection>;
}
