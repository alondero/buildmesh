import { useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { useClickOutside } from '../../hooks/useClickOutside';

export function PromptActions({ name, custom, disabled, onEdit, onDuplicate, onDelete }: {
  name: string; custom: boolean; disabled: boolean;
  onEdit: () => void; onDuplicate: () => void; onDelete: () => void;
}) {
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const close = () => { setOpen(false); trigger.current?.focus(); };
  useClickOutside(open ? id : null, () => setOpen(false));
  useAnchoredPosition(trigger, menu, open, { align: 'end' });
  useAriaMenu({ rootRef: menu, activeIndex, setActiveIndex, enabled: open, onClose: close });
  const actions = [
    ...(custom ? [{ label: 'Edit saved prompt', run: onEdit }] : []),
    { label: 'Duplicate', run: onDuplicate },
    ...(custom ? [{ label: 'Delete prompt', run: onDelete }] : []),
  ];
  return <div data-dropdown-for={id}>
    <button ref={trigger} type="button" disabled={disabled} aria-label={`Actions for ${name}`}
      aria-haspopup="menu" aria-expanded={open} aria-controls={open ? id : undefined}
      onClick={() => setOpen(value => !value)}
      className="min-h-7 min-w-6 rounded-md px-1 text-text-secondary hover:bg-bg-card-hover disabled:opacity-40">⋯</button>
    {open && createPortal(<div ref={menu} id={id} role="menu" aria-label={`Actions for ${name}`} data-dropdown-for={id}
      className="fixed z-[100] w-44 max-w-[calc(100vw-8px)] rounded-md border border-border-default bg-bg-surface p-1 shadow-lg">
      {actions.map((action, index) => <button key={action.label} role="menuitem" type="button"
        tabIndex={activeIndex === index ? 0 : -1} onFocus={() => setActiveIndex(index)}
        onMouseEnter={event => event.currentTarget.focus()}
        onClick={() => { close(); action.run(); }}
        className={`block min-h-6 w-full rounded-md px-2 py-1.5 text-left text-xs ${activeIndex === index ? 'bg-bg-selection text-text-primary' : 'text-text-secondary'}`}>
        {action.label}
      </button>)}
    </div>, document.body)}
  </div>;
}
