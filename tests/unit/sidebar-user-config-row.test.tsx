/**
 * UserConfigRow — issue #60. Sidebar entry that opens the User Config File
 * Explorer at the resolved ~/.claude directory.
 *
 * Why a row (not just a button)
 * -----------------------------
 * Visual parity with the meshes section: the User Config row should read
 * as "another thing the sidebar can browse," not "a one-off shortcut."
 * The row is purely presentational — it owns no state and never renders
 * children. The click handler reads visibility from `useUIStore` (the
 * single source of truth shared with the UserConfigPanel) and toggles it.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { UserConfigRow } from '../../src/components/Sidebar/UserConfigRow';
import { useUIStore } from '../../src/stores/uiStore';

describe('UserConfigRow (#60)', () => {
  beforeEach(() => {
    // Reset shared UI state so a sibling test toggling the panel open
    // doesn't bleed in (zustand store outlives RTL's auto-cleanup by
    // default; same pattern as sidebar-mesh-item.test.tsx:107-125).
    useUIStore.setState({
      userConfigOpen: false,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it('renders the label and a folder icon button', () => {
    render(<UserConfigRow />);
    expect(screen.getByText('User Config')).toBeTruthy();
    // Folder-open button has the accessible name "Open user config" so a
    // screen reader announces the intent, not just the glyph. This is the
    // one click target the row exposes.
    expect(screen.getByRole('button', { name: 'Open user config' })).toBeTruthy();
  });

  it('toggles userConfigOpen when the folder button is clicked', async () => {
    render(<UserConfigRow />);
    expect(useUIStore.getState().userConfigOpen).toBe(false);

    await userEvent.click(screen.getByRole('button', { name: 'Open user config' }));
    expect(useUIStore.getState().userConfigOpen).toBe(true);

    await userEvent.click(screen.getByRole('button', { name: 'Open user config' }));
    expect(useUIStore.getState().userConfigOpen).toBe(false);
  });
});
