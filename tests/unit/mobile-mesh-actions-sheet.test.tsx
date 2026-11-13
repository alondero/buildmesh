/**
 * MeshActionsSheet — the phone's "More actions" sheet for one mesh.
 *
 * Issue #2776 — the sheet rendered without a label, so the accessibility
 * tree announced a bare dialog: a screen-reader user could not tell which
 * mesh the actions belonged to. The repair names the dialog with the mesh
 * identity through the Sheet's supported `label` prop.
 *
 * The sheet's own keyboard contract is pinned here too, because naming a
 * dialog is only useful if the rest of its behaviour survives: focus moves
 * into the sheet on open, Tab wraps inside it, Escape closes it, and focus
 * returns to whatever opened it.
 */

import { describe, it, expect, vi, beforeAll, afterAll } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { MeshActionsSheet } from '../../src/mobile/components/nodeList/MeshActionsSheet';
import type { Mesh } from '../../src/mobile/api';

/**
 * jsdom has no layout, so `offsetParent` reads null here. The Sheet asks it
 * "is this control visible?" when it collects the focusable set for its Tab
 * trap, so without the shim the trap sees only the focused control and does
 * not wrap — the wrap assertion below fails without it. The shim answers the
 * property the way a browser does — a visible element's offset parent is its
 * positioned ancestor or its parent — which is what the trap is written
 * against.
 */
const originalOffsetParent = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetParent');
beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, 'offsetParent', {
    configurable: true,
    get(this: HTMLElement) {
      return this.parentElement;
    },
  });
});
afterAll(() => {
  if (originalOffsetParent) Object.defineProperty(HTMLElement.prototype, 'offsetParent', originalOffsetParent);
  else delete (HTMLElement.prototype as unknown as Record<string, unknown>).offsetParent;
});

function makeMesh(name: string): Mesh {
  return {
    id: 1,
    name,
    path: '/tmp/repo',
    created_at: '2026-06-11T00:00:00Z',
    scratchpad: '',
    sandbox: false,
  };
}

const noop = () => {};

describe('MeshActionsSheet dialog name (issue #2776)', () => {
  it('names the dialog with the mesh identity', () => {
    render(
      <MeshActionsSheet
        mesh={makeMesh('buildmesh')}
        onClose={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
      />,
    );
    // The accessible name is the mesh the actions belong to.
    expect(screen.getByRole('dialog', { name: 'Actions for buildmesh' })).toBeTruthy();
  });

  it('names the dialog with a second mesh, so the name is not hard-coded', () => {
    render(
      <MeshActionsSheet
        mesh={makeMesh('playground')}
        onClose={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
      />,
    );
    expect(screen.getByRole('dialog', { name: 'Actions for playground' })).toBeTruthy();
    expect(screen.queryByRole('dialog', { name: 'Actions for buildmesh' })).toBeNull();
  });

  it('keeps a long mesh name intact in the dialog name', () => {
    const long = 'a-very-long-mesh-name-that-should-not-be-truncated-in-the-accessible-name';
    render(
      <MeshActionsSheet
        mesh={makeMesh(long)}
        onClose={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
      />,
    );
    expect(screen.getByRole('dialog', { name: `Actions for ${long}` })).toBeTruthy();
  });

  it('still shows the mesh name as the sheet heading', () => {
    render(
      <MeshActionsSheet
        mesh={makeMesh('buildmesh')}
        onClose={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
      />,
    );
    const dialog = screen.getByRole('dialog', { name: 'Actions for buildmesh' });
    expect(dialog.textContent).toContain('buildmesh');
  });
});

describe('MeshActionsSheet keyboard contract', () => {
  it('moves focus into the sheet on open and restores it on close', () => {
    const opener = document.createElement('button');
    opener.textContent = 'More actions';
    document.body.appendChild(opener);
    opener.focus();
    expect(document.activeElement).toBe(opener);

    const onClose = vi.fn();
    const { unmount } = render(
      <MeshActionsSheet
        mesh={makeMesh('buildmesh')}
        onClose={onClose}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
      />,
    );

    // Focus moved into the sheet, onto its first control.
    const first = screen.getByRole('button', { name: /Pull requests|Archive/ });
    expect(document.activeElement).toBe(first);

    unmount();
    // …and it comes back to the opener when the sheet goes away.
    expect(document.activeElement).toBe(opener);
    cleanup();
    opener.remove();
  });

  it('wraps Tab inside the sheet instead of walking into the page behind it', () => {
    render(
      <MeshActionsSheet
        mesh={makeMesh('buildmesh')}
        onClose={noop}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
        onOpenPulls={noop}
      />,
    );
    const dialog = screen.getByRole('dialog');
    const focusable = Array.from(
      dialog.querySelectorAll<HTMLElement>('button:not([disabled])'),
    );
    expect(focusable.length).toBeGreaterThan(1);

    // Focus the last control and Tab: the sheet keeps focus inside itself.
    const last = focusable[focusable.length - 1];
    last.focus();
    fireEvent.keyDown(dialog, { key: 'Tab' });
    expect(document.activeElement).toBe(focusable[0]);

    // Shift+Tab from the first control wraps to the last one.
    fireEvent.keyDown(dialog, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it('closes on Escape', () => {
    const onClose = vi.fn();
    render(
      <MeshActionsSheet
        mesh={makeMesh('buildmesh')}
        onClose={onClose}
        onOpenAgentNodes={noop}
        onOpenIssues={noop}
      />,
    );
    const dialog = screen.getByRole('dialog');
    fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
