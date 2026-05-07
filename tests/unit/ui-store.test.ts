import { describe, it, expect, beforeEach } from 'vitest';
import { useUIStore } from '../../src/stores/uiStore';

describe('useUIStore', () => {
  beforeEach(() => {
    useUIStore.setState({ changedFilesOpen: false, changedFilesNodeId: null });
  });

  describe('toggleChangedFiles', () => {
    it('opens panel for a node', () => {
      useUIStore.getState().toggleChangedFiles(1);
      expect(useUIStore.getState().changedFilesOpen).toBe(true);
      expect(useUIStore.getState().changedFilesNodeId).toBe(1);
    });

    it('closes panel when toggling same node', () => {
      useUIStore.getState().toggleChangedFiles(1);
      useUIStore.getState().toggleChangedFiles(1);
      expect(useUIStore.getState().changedFilesOpen).toBe(false);
      expect(useUIStore.getState().changedFilesNodeId).toBe(null);
    });

    it('switches to new node when different node clicked', () => {
      useUIStore.getState().toggleChangedFiles(1);
      useUIStore.getState().toggleChangedFiles(2);
      expect(useUIStore.getState().changedFilesOpen).toBe(true);
      expect(useUIStore.getState().changedFilesNodeId).toBe(2);
    });
  });

  describe('closeChangedFiles', () => {
    it('resets both state fields', () => {
      useUIStore.getState().toggleChangedFiles(5);
      useUIStore.getState().closeChangedFiles();
      expect(useUIStore.getState().changedFilesOpen).toBe(false);
      expect(useUIStore.getState().changedFilesNodeId).toBe(null);
    });
  });

  describe('setChangedFilesNodeId', () => {
    it('updates node when panel is open', () => {
      useUIStore.getState().toggleChangedFiles(1);
      useUIStore.getState().setChangedFilesNodeId(3);
      expect(useUIStore.getState().changedFilesNodeId).toBe(3);
      expect(useUIStore.getState().changedFilesOpen).toBe(true);
    });

    it('does nothing when panel is closed', () => {
      useUIStore.getState().setChangedFilesNodeId(3);
      expect(useUIStore.getState().changedFilesNodeId).toBe(null);
      expect(useUIStore.getState().changedFilesOpen).toBe(false);
    });
  });
});
