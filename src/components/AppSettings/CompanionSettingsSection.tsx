import { useState } from 'react';
import { saveCompanionSettings } from '../../lib/tauri/companion';
import { formatError } from '../../lib/errorUtils';
import { SettingsRow, SettingsSection } from './SettingsRow';
import { useSettingsData } from './SettingsDataContext';
import type { CompanionSettingsUpdate } from '../../types/generated/CompanionSettingsUpdate';

export function CompanionSettingsSection() {
  const { preferences, prefsLoaded, loadPreferences, setError } = useSettingsData();
  const [busy, setBusy] = useState(false);
  const settings = preferences?.companion ?? { launch_configuration: null, spawn_limit: 5 };
  const save = async (next: CompanionSettingsUpdate) => {
    setBusy(true);
    try { await saveCompanionSettings(next); await loadPreferences(); }
    catch (error) { setError(formatError(error)); }
    finally { setBusy(false); }
  };
  return <SettingsSection title="Companion AI">
    <p className="text-sm text-text-secondary">Choose the AI Assistant Launch Configuration in Providers &gt; Provider defaults.</p>
    <SettingsRow label="Maximum nodes per task" htmlFor="companion-limit" summary="Default 5. Hard limit 10, shared across every spawn in a task.">
      <select id="companion-limit" disabled={!prefsLoaded || busy} value={settings.spawn_limit}
        className="rounded-md border border-border-default bg-bg-card p-2 text-text-primary"
        onChange={event => void save({ spawn_limit: Number(event.target.value) })}>
        {Array.from({ length: 10 }, (_, index) => index + 1).map(limit => <option key={limit}>{limit}</option>)}
      </select>
    </SettingsRow>
  </SettingsSection>;
}
