import { useCallback, useEffect, useState } from 'react';
import { type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { runUpdateCheck } from '../lib/updater';

export interface UpdateCheckState {
  /** True when an update is pending and the user hasn't dismissed the prompt. */
  available: boolean;
  update: Update | null;
  installing: boolean;
  /** Download + install the pending update, then relaunch into the new version. */
  install: () => Promise<void>;
  /** Dismiss the prompt for this session ("Later"). */
  dismiss: () => void;
}

// Checks once on mount (guarded to production Tauri builds inside
// `runUpdateCheck`) and drives the <UpdatePrompt> dialog. Kept separate from
// the presentational component so the check/install wiring is testable via
// plugin mocks. Issue #826.
export function useUpdateCheck(): UpdateCheckState {
  const [update, setUpdate] = useState<Update | null>(null);
  const [installing, setInstalling] = useState(false);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    runUpdateCheck().then((u) => {
      if (!cancelled && u) setUpdate(u);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const install = useCallback(async () => {
    if (!update) return;
    setInstalling(true);
    try {
      await update.downloadAndInstall();
      await relaunch();
    } catch (e) {
      // Surface in the log bridge; re-enable the button so the user can retry.
      console.error('[updater] install failed:', e);
      setInstalling(false);
    }
  }, [update]);

  const dismiss = useCallback(() => setDismissed(true), []);

  return {
    available: !!update && !dismissed,
    update,
    installing,
    install,
    dismiss,
  };
}
