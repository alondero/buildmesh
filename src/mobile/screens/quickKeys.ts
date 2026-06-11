// Quick-key strip for the mobile terminal. Soft keyboards have no Esc, Tab,
// arrows or Ctrl — yet agent CLIs are driven by exactly those keys (Esc to
// interrupt, arrows + Enter for menus, Shift+Tab for Claude Code's mode
// cycling). Sequences are the standard xterm encodings a hardware keyboard
// would produce.

export interface QuickKey {
  id: string;
  label: string;
  seq: string;
}

export const QUICK_KEYS: QuickKey[] = [
  { id: "esc", label: "esc", seq: "\x1b" },
  { id: "tab", label: "tab", seq: "\t" },
  { id: "shift-tab", label: "⇧tab", seq: "\x1b[Z" },
  { id: "up", label: "↑", seq: "\x1b[A" },
  { id: "down", label: "↓", seq: "\x1b[B" },
  { id: "left", label: "←", seq: "\x1b[D" },
  { id: "right", label: "→", seq: "\x1b[C" },
  { id: "enter", label: "⏎", seq: "\r" },
  { id: "ctrl-c", label: "^C", seq: "\x03" },
];
