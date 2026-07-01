/**
 * Sidebar width persistence (design-system pass): the width previously reset
 * to 256px on every launch. It now round-trips through localStorage, clamped
 * so a stale or corrupted value can't wedge the sidebar off-screen.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { render, cleanup, act } from '@testing-library/react';
import { useSidebarResize } from '../../src/components/Sidebar/useSidebarResize';

const STORAGE_KEY = 'buildmesh.sidebar-width';

function mount() {
  const captured: { current: ReturnType<typeof useSidebarResize> | null } = { current: null };
  function Probe() {
    captured.current = useSidebarResize();
    return null;
  }
  const utils = render(<Probe />);
  return { captured, ...utils };
}

beforeEach(() => window.localStorage.clear());
afterEach(cleanup);

describe('useSidebarResize persistence', () => {
  it('starts from the persisted width instead of the default', () => {
    window.localStorage.setItem(STORAGE_KEY, '320');
    const { captured } = mount();
    expect(captured.current!.width).toBe(320);
  });

  it('falls back to the default when nothing is stored', () => {
    const { captured } = mount();
    expect(captured.current!.width).toBe(256);
  });

  it('clamps a stored width outside [192, 480] so a bad value cannot wedge the sidebar', () => {
    window.localStorage.setItem(STORAGE_KEY, '9999');
    expect(mount().captured.current!.width).toBe(480);
    cleanup();
    window.localStorage.setItem(STORAGE_KEY, '1');
    expect(mount().captured.current!.width).toBe(192);
  });

  it('ignores a garbage stored value', () => {
    window.localStorage.setItem(STORAGE_KEY, 'not-a-number');
    expect(mount().captured.current!.width).toBe(256);
  });

  it('persists the settled width', async () => {
    mount();
    // The persist effect runs whenever width settles (isResizing false);
    // the initial mount itself writes the current width.
    await act(async () => {});
    expect(window.localStorage.getItem(STORAGE_KEY)).toBe('256');
  });
});
