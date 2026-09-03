import { useRef, useState } from 'react';
import type { AgentNode } from '../../stores/agentNodeStore';
import type { SpawnOption } from '../../lib/groups';
import { RegenerateProviderMenu } from '../Providers/RegenerateProviderMenu';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { useViewportClamp } from '../../hooks/useViewportClamp';
import { dropdownId } from '../../lib/dropdownId';

interface GridRegenerateButtonProps {
  node: AgentNode;
  providerList: SpawnOption[];
  isDisabled: boolean;
  hasTargets: boolean;
  onPick: (providerId: string, providerLabel: string) => void;
}

/**
 * Issue #1502 — Regenerate toolbar button for `GridNodeHeader`.
 *
 * Rendered inline (next to Build/Run) at `xl` / `wide` / `medium` tiers
 * (`>= 380px`, i.e. whenever `showInlineActions` is true). At `slim` /
 * `compact` the same picker lives inside the kebab overflow menu instead
 * (see `KebabActions` in `GridNodeHeader.tsx`) — the button follows the
 * existing inline/kebab boundary rather than introducing a second
 * breakpoint, so the responsive-tier tests keep asserting "no kebab at
 * medium+" without a carve-out.
 *
 * Behaviour mirrors `BuildRunDropdown`: icon-only 28×28 trigger with the
 * same `bg-bg-base/60 + border` surface so the trio reads as one control
 * group, absolute dropdown below with the shared
 * `RegenerateProviderMenu` (current pinned on top for in-place
 * kick-start, then alternates grouped by harness). Keyboard contract via
 * the shared `useAriaMenu` hook (Arrow/ Home/End + Escape + Tab) and
 * viewport clamping via `useViewportClamp`.
 */
export function GridRegenerateButton({
  node,
  providerList,
  isDisabled,
  hasTargets,
  onPick,
}: GridRegenerateButtonProps) {
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  // One row per provider (current pinned on top inside the menu) — a
  // plain length, no partition needed.
  const itemCount = providerList.length;

  const disabled = isDisabled || !hasTargets;

  const closeAndReturnFocus = () => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  };

  useClickOutside<string>(open ? dropdownId('grid-regen', node.id) : null, () => setOpen(false));

  useAriaMenu({
    rootRef: menuRef,
    itemCount,
    activeIndex,
    setActiveIndex,
    onClose: closeAndReturnFocus,
    enabled: open,
  });

  useViewportClamp(menuRef, [open]);

  const handlePick = (providerId: string, providerLabel: string) => {
    setOpen(false);
    onPick(providerId, providerLabel);
  };

  return (
    <div
      className="relative"
      data-dropdown-for={open ? dropdownId('grid-regen', node.id) : undefined}
    >
      <button
        ref={triggerRef}
        type="button"
        onClick={() => {
          if (disabled) return;
          setOpen((o) => !o);
        }}
        disabled={disabled}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="Regenerate agent node"
        title={
          isDisabled
            ? `Regenerate unavailable while ${node.status}`
            : !hasTargets
              ? 'No providers are available on this mesh'
              : 'Regenerate agent node (including current to kick-start)'
        }
        data-testid="grid-regenerate-button"
        className="w-7 h-7 flex items-center justify-center rounded-md bg-bg-base/60 border border-border-default text-text-primary hover:text-accent-amber hover:bg-accent-amber/15 hover:border-accent-amber/60 transition-colors disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-bg-base/60 disabled:hover:text-text-primary disabled:hover:border-border-default"
      >
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" />
          <path d="M21 3v5h-5" />
          <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" />
          <path d="M3 21v-5h5" />
        </svg>
      </button>

      {open && (
        <div
          ref={menuRef}
          role="menu"
          aria-label="Pick target provider"
          data-testid="grid-regenerate-submenu"
          className="absolute right-0 top-full mt-1 min-w-[220px] bg-bg-overlay border border-border-default rounded-md shadow-md py-1 z-50 animate-scale-in origin-top-right"
        >
          <RegenerateProviderMenu
            providers={providerList}
            currentProviderId={node.provider}
            onPick={handlePick}
            submenuTestId="grid-regenerate-submenu"
            activeIndex={activeIndex}
          />
        </div>
      )}
    </div>
  );
}
