import { describe, it, expect } from 'vitest';
import { computeDropIntent } from '../../src/components/AgentNodeView/nodeDrag';

// Geometry shared by most cases: target node occupies x ∈ [0, 100].
const base = {
  overNodeId: 2,
  overMeshId: 1,
  overRectLeft: 0,
  overRectWidth: 100,
  draggedId: 1,
  draggedMeshId: 1,
};

describe('computeDropIntent', () => {
  it('returns null when there is no node under the pointer', () => {
    expect(computeDropIntent({ ...base, overNodeId: null, overMeshId: null, pointerX: 50 })).toBeNull();
  });

  it('refuses cross-mesh drops (same-mesh reordering only)', () => {
    expect(computeDropIntent({ ...base, overMeshId: 9, pointerX: 50 })).toBeNull();
  });

  it('left third = insert before the target', () => {
    expect(computeDropIntent({ ...base, pointerX: 20 })).toEqual({ kind: 'insert-before', targetNodeId: 2 });
  });

  it('right third = insert after the target', () => {
    expect(computeDropIntent({ ...base, pointerX: 80 })).toEqual({ kind: 'insert-after', targetNodeId: 2 });
  });

  it('middle = swap with the target', () => {
    expect(computeDropIntent({ ...base, pointerX: 50 })).toEqual({ kind: 'swap', targetNodeId: 2 });
  });

  it('a node cannot swap with itself (middle over the dragged node)', () => {
    expect(computeDropIntent({ ...base, overNodeId: 1, pointerX: 50 })).toBeNull();
  });

  it('respects the rect offset when computing the zone ratio', () => {
    // Target shifted to x ∈ [200, 300]; pointer at 290 is in the right third.
    expect(computeDropIntent({ ...base, overRectLeft: 200, overRectWidth: 100, pointerX: 290 }))
      .toEqual({ kind: 'insert-after', targetNodeId: 2 });
  });
});
