/**
 * The two modal-wide surfaces that sit *above* the panes: the dirty-site
 * aggregator (issue #730) and the shared error banner.
 *
 * These are separated from `AppSettingsModal` itself because of where they
 * live in the tree. The dirty set is the shell's own transition state and
 * must be readable by the nav rail and the `<Modal dirty>` prop, while the
 * error banner and the recovery panel need to be *inside*
 * `SettingsDataProvider` to read the shared context. Keeping both here lets
 * `AppSettingsModal` compose the provider around a shell that consumes it —
 * a component cannot wrap itself in its own provider.
 */
import { useCallback, useState } from 'react';
import { useSettingsData } from './SettingsDataContext';

/**
 * Modal-wide dirty aggregator (issue #730). Every child that can be edited
 * (AccountCard, AddProviderForm, HarnessConfigList, the pool/worktree
 * drafts) reports via `siteDirtyChange(site, dirty)`; the Set's size feeds
 * the Modal's `dirty` prop so a stray Escape or backdrop click is
 * intercepted by the inline "Discard unsaved changes?" banner.
 *
 * The function-form setState bails out when the site is already in the
 * right state, so re-fires from a non-memoised child callback are cheap.
 *
 * Invariant ownership: this setter is the *only* way a pane can touch the
 * set, and it lives next to the state it guards — a pane cannot read or
 * write `dirtySites` directly, so the "any pane is dirty ⇒ modal is dirty"
 * rule cannot be bypassed by a second entrypoint (issue #1002).
 */
export function useDirtySites() {
  const [dirtySites, setDirtySites] = useState<Set<string>>(new Set());

  const siteDirtyChange = useCallback((site: string, dirty: boolean) => {
    setDirtySites(prev => {
      if (dirty) {
        if (prev.has(site)) return prev;
        const next = new Set(prev);
        next.add(site);
        return next;
      }
      if (!prev.has(site)) return prev;
      const next = new Set(prev);
      next.delete(site);
      return next;
    });
  }, []);

  return { dirtySites, siteDirtyChange };
}

/**
 * Shared error surface — outside the panes so a failed save is visible no
 * matter which pane the user is looking at. Panes call `setError` from the
 * shared context; only this component renders the result.
 */
export function SettingsError() {
  const { error } = useSettingsData();
  if (!error) return null;
  return <div className="mb-4 text-status-error text-base">{error}</div>;
}