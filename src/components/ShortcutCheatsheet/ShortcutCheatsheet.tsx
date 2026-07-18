import { useRef } from 'react';
import { Modal, ModalCloseButton } from '../shared/Modal';
import {
  groupedShortcutEntries,
  shortcutLabel,
  type ShortcutGroup,
} from '../../lib/shortcutCatalog';

interface ShortcutCheatsheetProps {
  /** Controlled open flag — the modal mounts only while true. */
  open: boolean;
  onClose: () => void;
}

const GROUP_DESCRIPTIONS: Record<ShortcutGroup, string> = {
  // The descriptions render as a one-line sub-heading under the group name
  // so a new user can tell at a glance *where* the keys apply without
  // reading the key column.
  app: 'Work anywhere in the app',
  grid: 'Traverse the on-screen node grid',
  terminal: 'Apply to a focused agent terminal',
  modal: 'Context-bound',
};

/**
 * Discoverable keyboard shortcut cheatsheet (issue #731).
 *
 * Lists every shortcut shipped in Buildmesh, grouped by the surface it
 * applies to (App / Grid / Terminal / Modal). Opened by `?` from anywhere
 * in the app, closed by Escape or the corner `×`.
 *
 * Render the modal conditionally — mounting is what arms the Escape
 * listener via the <Modal> primitive, so a permanently-mounted instance
 * would steal Escape from agent terminals. App.tsx owns the open-state
 * and the `?`-key window listener; this component is the visible half.
 */
export function ShortcutCheatsheet({ open, onClose }: ShortcutCheatsheetProps) {
  // Ref pointing at the close button. Passed to <Modal> as `defaultFocusRef`
  // so the modal focuses the close button on mount — Issue #731 acceptance
  // criterion "focus the close button by default": the user just opened
  // this dialog, the only useful action is to close it, so the close
  // button is the right default target. Enter / Space then dismisses.
  //
  // Issue #748: replaced the earlier `useEffect` that focused the close
  // button (the Modal's effect focused the panel first, then this effect
  // overrode it — order-dependent and child-effect-before-parent-effect
  // fragile). The Modal now exposes `defaultFocusRef` so the focus target
  // is set declaratively in JSX, with no effect ordering to reason about.
  const closeBtnRef = useRef<HTMLButtonElement>(null);

  if (!open) return null;

  const groups = groupedShortcutEntries();

  return (
    <Modal
      onClose={onClose}
      labelledBy="shortcut-cheatsheet-title"
      maxWidth="max-w-md"
      className="p-0"
      defaultFocusRef={closeBtnRef}
    >
      <div className="flex items-start justify-between gap-4 px-5 pt-5 pb-3 border-b border-border-subtle">
        <div>
          <h2 id="shortcut-cheatsheet-title" className="text-lg font-semibold text-text-primary">
            Keyboard shortcuts
          </h2>
          <p className="text-xs text-text-muted mt-0.5">
            Press <Kbd>?</Kbd> to reopen this dialog.
          </p>
        </div>
        <ModalCloseButton onClose={onClose} label="Close" ref={closeBtnRef} />
      </div>

      <div className="px-5 py-4 max-h-[70vh] overflow-y-auto">
        {groups.map(group => (
          <section key={group.group} className="mb-5 last:mb-0">
            <header className="mb-2">
              <h3 className="text-2xs font-bold uppercase tracking-wide text-text-secondary">
                {group.title}
              </h3>
              <p className="text-2xs text-text-muted mt-0.5">{GROUP_DESCRIPTIONS[group.group]}</p>
            </header>
            <ul className="space-y-1.5">
              {group.entries.map(entry => (
                <li
                  key={entry.action}
                  className="flex items-center justify-between gap-3 text-xs"
                >
                  <span className="text-text-primary">{entry.description}</span>
                  <Kbd>{shortcutLabel(entry)}</Kbd>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </Modal>
  );
}

/**
 * The <kbd> pill used inside this cheatsheet. The class string is
 * intentionally identical to the empty-state splash's `<kbd>` pills
 * (`AgentNodeView.tsx` empty-state splash, issue #668 adopted the same
 * shape) so a user who learns the look in one surface recognises it in
 * the other — the discoverability invariant that the splash hint relies
 * on. If the splash's class string ever changes, change it here too.
 */
function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="px-1 py-0.5 rounded-md bg-bg-card border border-border-default text-xs text-text-muted font-mono">
      {children}
    </kbd>
  );
}
