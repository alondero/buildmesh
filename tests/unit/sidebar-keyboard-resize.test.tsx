/**
 * The sidebar's resize handle, operable by keyboard (issue #2424).
 *
 * The sidebar is the app's left dock; its handle used to answer the mouse
 * only. These tests drive the REAL `Sidebar` and assert what a user can
 * observe: the handle is a named separator in the tab order that states
 * its range, the arrow keys move it inside the SAME [192, 480] bounds the
 * drag enforces, and the chosen width is persisted to localStorage the
 * way a dragged width is.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup, act } from '@testing-library/react';
import { Sidebar } from '../../src/components/Sidebar/Sidebar';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

vi.mock('../../src/hooks/useProviderList', () => ({
  useProviderList: () => [],
  useProviderReadiness: () => ({ status: 'loaded', error: null }),
  retryProviderList: () => undefined,
  __resetSharedProviderListForTests: () => undefined,
}));

const STORAGE_KEY = 'buildmesh.sidebar-width';

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
} as Mesh;

function sidebarSeparator(): HTMLElement {
  return screen.getByRole('separator', { name: 'Resize sidebar' });
}

beforeEach(() => {
  window.localStorage.clear();
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  seedAgentNodes([]);
  useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
});

afterEach(cleanup);

describe('the sidebar resize separator (issue #2424)', () => {
  it('is a named, focusable separator that states its range', () => {
    render(<Sidebar />);

    const separator = sidebarSeparator();
    expect(separator.getAttribute('aria-orientation')).toBe('vertical');
    expect(separator.getAttribute('aria-valuenow')).toBe('256');
    expect(separator.getAttribute('aria-valuemin')).toBe('192');
    expect(separator.getAttribute('aria-valuemax')).toBe('480');
    expect(separator.getAttribute('tabindex')).toBe('0');
  });

  it('resizes with the arrow keys — right grows, left shrinks', () => {
    render(<Sidebar />);
    const separator = sidebarSeparator();

    fireEvent.keyDown(separator, { key: 'ArrowRight' });
    expect(separator.getAttribute('aria-valuenow')).toBe('264');

    fireEvent.keyDown(separator, { key: 'ArrowLeft' });
    expect(separator.getAttribute('aria-valuenow')).toBe('256');
  });

  it('reaches the minimum and the maximum and stops there', () => {
    render(<Sidebar />);
    const separator = sidebarSeparator();

    fireEvent.keyDown(separator, { key: 'End' });
    expect(separator.getAttribute('aria-valuenow')).toBe('192');

    // At the minimum, growing is a no-op — the bound holds.
    fireEvent.keyDown(separator, { key: 'ArrowLeft' });
    expect(separator.getAttribute('aria-valuenow')).toBe('192');

    fireEvent.keyDown(separator, { key: 'Home' });
    expect(separator.getAttribute('aria-valuenow')).toBe('480');

    // At the maximum, shrinking is a no-op — the bound holds.
    fireEvent.keyDown(separator, { key: 'ArrowRight' });
    expect(separator.getAttribute('aria-valuenow')).toBe('480');
  });

  it('persists the keyboard-chosen width to localStorage', async () => {
    render(<Sidebar />);

    fireEvent.keyDown(sidebarSeparator(), { key: 'ArrowRight' });
    await act(async () => {});

    expect(window.localStorage.getItem(STORAGE_KEY)).toBe('264');
  });

  it('restores a keyboard-chosen width on the next launch', () => {
    window.localStorage.setItem(STORAGE_KEY, '320');
    render(<Sidebar />);

    expect(sidebarSeparator().getAttribute('aria-valuenow')).toBe('320');
  });

  it('still drags with the pointer after a keyboard resize', () => {
    render(<Sidebar />);
    const separator = sidebarSeparator();

    // The keyboard moved it first — the pointer path must take over from
    // the value the keyboard left behind, not from a stale one.
    fireEvent.keyDown(separator, { key: 'ArrowRight' });
    expect(separator.getAttribute('aria-valuenow')).toBe('264');

    fireEvent(separator, new MouseEvent('mousedown', { bubbles: true, cancelable: true, clientX: 100 }));
    fireEvent(document, new MouseEvent('mousemove', { bubbles: true, clientX: 140 }));
    fireEvent(document, new MouseEvent('mouseup', { bubbles: true }));

    expect(separator.getAttribute('aria-valuenow')).toBe('304');
  });
});
