import { create } from 'zustand';

// The Universal Command Omnibar (issue #1409) — the single source of truth
// for the palette's open/close state. A store (rather than a local state in
// whatever component first renders the palette) because the ⌘/Ctrl+K and
// ⌘/Ctrl+P global shortcuts dispatch from App.tsx and any future entry point
// (TitleBar button, splash row) must route through the same place.
//
// `mode` is the preselected palette mode: 'files' is the default search
// (⌘/Ctrl+K), 'commands' is the command-runner (⌘/Ctrl+P) — the editors'
// "quick open file / run command" convention. The palette UI reads `mode`
// once at mount to seed its search box; the user can switch modes inside
// the palette without touching this store.
export type OmnibarMode = 'files' | 'commands';

interface OmnibarState {
  /** True while the palette is mounted. */
  open: boolean;
  /** The mode the palette should seed itself with. */
  mode: OmnibarMode;
  /** Open the palette, preselecting a mode (defaults to 'files'). */
  openOmnibar: (mode?: OmnibarMode) => void;
  /** Close the palette. */
  closeOmnibar: () => void;
}

export const useOmnibarStore = create<OmnibarState>((set, get) => ({
  open: false,
  mode: 'files',

  openOmnibar: (mode = 'files') => {
    // Opening an already-open palette just re-seeds the mode — the user
    // pressed ⌘/Ctrl+P while the palette was up, so switch it to command
    // mode rather than stacking a second palette.
    set({ open: true, mode });
  },

  closeOmnibar: () => {
    if (!get().open) return;
    set({ open: false });
  },
}));
