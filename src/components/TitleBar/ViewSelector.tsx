import { useCallback, useId, useLayoutEffect, useRef, useState } from 'react';

import { useUIStore, type ViewMode } from '../../stores/uiStore';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { dropdownId } from '../../lib/dropdownId';
import { VIEW_MODE_SEGMENTS, selectViewMode } from '../ViewModeSwitcher/ViewModeSwitcher';
import { CheckGlyph, ChevronGlyph, MeshPickerRows, useOpenOnPickerRequest, useScopeLabel } from './ScopeIndicator';

/**
 * ViewSelector — the title bar's View Mode and scope control below
 * `LABELLED_SWITCHER_MIN_WIDTH` (#2421), in place of the five segments and
 * the `ScopeIndicator` beside them.
 *
 * The trigger always names the active View Mode and its scope in visible
 * text ("Mesh Grid · demo-1", "All Nodes · all meshes · 3 nodes"), so a
 * narrow window shows where the user is without hover — the segments it
 * replaces had collapsed to five unlabelled glyphs, next to a scope glyph
 * shaped like two of them.
 *
 * The panel holds both halves: the View Modes (`aria-pressed` on the active
 * one, as on the segments) and the Mesh picker (`aria-current`, as in
 * `ScopeIndicator`). Choosing either goes through the same functions the wide
 * controls call — `selectViewMode` and `enterMesh` — so the two tiers cannot
 * disagree about what a choice means.
 *
 * Disclosure contract, shared with `ScopeIndicator`, `TitleBarOverflow` and
 * `ZoomControl`: a `role="dialog"` of plain buttons, focus moves into the
 * panel on open, and Escape closes it and returns focus to the trigger. An
 * outside click closes it and keeps focus where the user clicked (returning
 * it to the trigger only when that click left focus on <body>). Two choices
 * do not return focus on Escape either, because they hand it on:
 *
 *   - Mesh Grid with no Mesh selected asks for one (`enterMeshGrid` bumps
 *     `openScopePickerRequest`, #2076). This control is the picker at this
 *     tier, so the panel stays open on its Mesh list.
 *   - Filtered requests focus for the node search (#1609); the search input
 *     mounts in the same commit and takes it.
 */

const PANEL_ID = dropdownId('titlebar', 'view-selector');

export function ViewSelector() {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const viewHeadingId = useId();
  const meshHeadingId = useId();

  const viewMode = useUIStore(s => s.viewMode);
  const { detail, tooltip, meshes, selectedMeshId } = useScopeLabel();
  const active = VIEW_MODE_SEGMENTS.find(segment => segment.mode === viewMode) ?? VIEW_MODE_SEGMENTS[0];
  const ActiveIcon = active.Icon;

  const closeAndReturnFocus = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  }, []);

  // An outside click closes the panel. Focus stays on whatever the user
  // clicked; only if that left focus on <body> does it come back to the
  // trigger (#2421 review).
  const closeFromOutsideClick = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => {
      const active = document.activeElement;
      if (active === null || active === document.body) trigger?.focus();
    });
  }, []);
  useClickOutside(open ? PANEL_ID : null, closeFromOutsideClick);
  useEscapeKey(closeAndReturnFocus, open);

  useLayoutEffect(() => {
    if (!open) return;
    panelRef.current?.focus();
  }, [open]);

  // The Mesh Grid "ask for a Mesh" request (#2076) — this tier's picker.
  useOpenOnPickerRequest(setOpen);

  const chooseView = (mode: ViewMode) => {
    const pickerRequest = useUIStore.getState().openScopePickerRequest;
    selectViewMode(mode);
    // A Mesh was requested: stay open on the Mesh list.
    if (useUIStore.getState().openScopePickerRequest !== pickerRequest) return;
    if (mode === 'filtered') {
      setOpen(false);
      return;
    }
    closeAndReturnFocus();
  };

  const label = `${active.label} · ${detail}`;

  return (
    <div className="relative flex min-w-0 items-center" data-dropdown-for={open ? PANEL_ID : undefined}>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen(value => !value)}
        data-testid="view-selector"
        // The accessible name is the visible text verbatim (Label in Name),
        // and survives the scope half truncating.
        aria-label={label}
        aria-expanded={open}
        aria-haspopup="dialog"
        aria-controls={open ? PANEL_ID : undefined}
        title={tooltip}
        // Styled as the switcher's active segment: this control IS the
        // active View Mode at this tier.
        className="inline-flex h-9 min-w-0 items-center gap-1.5 rounded-md bg-bg-card px-2 py-1.5 text-sm font-sans font-medium transition-colors hover:text-text-primary"
      >
        <ActiveIcon className="h-4 w-4 shrink-0 text-accent-cyan" />
        <span className="shrink-0 whitespace-nowrap text-accent-cyan">{active.label}</span>
        <span aria-hidden className="shrink-0 text-text-muted">·</span>
        <span className="min-w-0 max-w-[10rem] truncate text-text-secondary">{detail}</span>
        <ChevronGlyph className="h-3.5 w-3.5 shrink-0 text-text-muted" />
      </button>
      {open && (
        <div
          ref={panelRef}
          id={PANEL_ID}
          role="dialog"
          tabIndex={-1}
          aria-label="View and scope"
          data-testid="view-selector-panel"
          className="absolute left-0 top-full z-50 mt-1 max-h-[calc(100dvh-4rem)] w-72 overflow-y-auto rounded-md border border-border-default bg-bg-card p-1 shadow-md animate-scale-in origin-top-left"
        >
          <div role="group" aria-labelledby={viewHeadingId}>
            <p id={viewHeadingId} className="px-2 pb-0.5 pt-1 text-2xs font-medium uppercase tracking-wide text-text-muted">
              View
            </p>
            {VIEW_MODE_SEGMENTS.map(({ mode, label: modeLabel, Icon }) => {
              const pressed = mode === viewMode;
              return (
                <button
                  key={mode}
                  type="button"
                  aria-pressed={pressed}
                  onClick={() => chooseView(mode)}
                  data-testid={`view-selector-mode-${mode}`}
                  className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-bg-overlay ${
                    pressed ? 'font-medium text-accent-cyan' : 'text-text-primary'
                  }`}
                >
                  <Icon className="h-4 w-4 shrink-0" />
                  {modeLabel}
                  {/* A glyph, not just the accent: colour is never the only
                      signal (the Mesh rows below carry the same check). */}
                  {pressed && <CheckGlyph className="ml-auto h-4 w-4 shrink-0" />}
                </button>
              );
            })}
          </div>
          <div className="mx-2 my-1 h-px bg-border-subtle" />
          <div role="group" aria-labelledby={meshHeadingId}>
            <p id={meshHeadingId} className="px-2 pb-0.5 pt-1 text-2xs font-medium uppercase tracking-wide text-text-muted">
              Mesh
            </p>
            <MeshPickerRows meshes={meshes} selectedMeshId={selectedMeshId} onPicked={closeAndReturnFocus} />
          </div>
        </div>
      )}
    </div>
  );
}
