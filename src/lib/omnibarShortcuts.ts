import { useOmnibarStore } from '../stores/omnibarStore';

// Omnibar dispatch (issue #1409). The ⌘/Ctrl+K and ⌘/Ctrl+P global
// shortcuts surface in App.tsx's `shortcut-triggered` handler as the
// `open-omnibar` / `open-omnibar-commands` actions; this pure mutator
// translates those action names into omnibarStore state changes so the
// handler stays a one-liner and the mapping is unit-testable without
// mounting the whole App (same discipline as gridShortcuts /
// awaitingInputShortcuts).

/** True for the Omnibar shortcut actions App.tsx dispatches (issue #1409). */
export function isOmnibarAction(action: string): boolean {
  return action === 'open-omnibar' || action === 'open-omnibar-commands';
}

export function handleOmnibarAction(action: string): void {
  if (!isOmnibarAction(action)) return;
  useOmnibarStore.getState().openOmnibar(action === 'open-omnibar-commands' ? 'commands' : 'files');
}
