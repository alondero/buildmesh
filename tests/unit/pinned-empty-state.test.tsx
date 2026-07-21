/**
 * PinnedEmptyState (wayfinder #982 / #986) — the styled empty state for
 * Pinned Grid mode when no nodes are pinned. Verifies the icon + copy
 * and the "View All Nodes" CTA: it must clear selectedMeshId (sync flips
 * the canvas to All Nodes), or — with no selection — call setViewMode
 * directly. Both routes land the user in All Nodes mode.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { PinnedEmptyState } from '../../src/components/AgentNodeView/AgentNodeView';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';

beforeEach(() => {
  useMeshStore.setState({
    meshes: [],
    meshesById: new Map(),
    selectedMeshId: null,
    loading: false,
    error: null,
  });
  useUIStore.setState({ viewMode: 'pinned', lastNonSingleMode: 'pinned' });
});

describe('PinnedEmptyState (wayfinder #982 / #986)', () => {
  it('renders the canonical heading and how-to-pin copy', () => {
    render(<PinnedEmptyState />);
    expect(screen.getByText('No pinned nodes')).toBeTruthy();
    expect(screen.getByText(/Pin agents from any mesh/)).toBeTruthy();
  });

  it('renders a "View All Nodes" call-to-action button', () => {
    render(<PinnedEmptyState />);
    const cta = screen.getByRole('button', { name: /view all nodes/i });
    expect(cta).toBeTruthy();
  });

  it('the CTA clears the sidebar selection when one is set — the sync flips the canvas to All', () => {
    // One-filter-two-controls: All Nodes ⇔ no mesh selected, so the
    // CTA goes through selectMesh(null). The uiStore mesh-subscription
    // does the setViewMode — the CTA never has to know.
    useMeshStore.setState({ selectedMeshId: 7 });
    render(<PinnedEmptyState />);
    fireEvent.click(screen.getByRole('button', { name: /view all nodes/i }));
    expect(useMeshStore.getState().selectedMeshId).toBeNull();
    expect(useUIStore.getState().viewMode).toBe('all');
  });

  it('the CTA falls back to setViewMode("all") when the sidebar has no selection', () => {
    // Nothing to clear — route the mode flip directly.
    render(<PinnedEmptyState />);
    fireEvent.click(screen.getByRole('button', { name: /view all nodes/i }));
    expect(useUIStore.getState().viewMode).toBe('all');
  });
});
