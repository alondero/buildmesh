/**
 * The saved navigation record (issue #2793) — its stored shape and the rules
 * that decide whether a stored record can be trusted.
 *
 * The record is the whole "where was I" position: the View Mode, the grid
 * mode Single was entered from, the selected Mesh and the active Agent Node.
 * A record that cannot be parsed is ignored rather than half-understood —
 * a lost position is better than a wrong one.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  NAVIGATION_SNAPSHOT_STORAGE_KEY,
  NAVIGATION_SNAPSHOT_VERSION,
  parseNavigationSnapshot,
  readNavigationSnapshot,
  writeNavigationSnapshot,
  type NavigationSnapshot,
} from '../../src/lib/navigationSnapshot';

const RECORD: NavigationSnapshot = {
  version: NAVIGATION_SNAPSHOT_VERSION,
  viewMode: 'mesh',
  gridMode: 'mesh',
  selectedMeshId: 2,
  selectedMeshName: 'playground',
  activeNodeId: 30,
  activeNodeName: 'scratch',
};

describe('the navigation snapshot record', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('round-trips a record through storage', () => {
    writeNavigationSnapshot(RECORD);
    expect(readNavigationSnapshot()).toEqual(RECORD);
  });

  it('reads nothing when no record was written', () => {
    expect(readNavigationSnapshot()).toBeNull();
  });

  it('ignores a record that is not JSON', () => {
    localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, '{not json');
    expect(readNavigationSnapshot()).toBeNull();
  });

  it('ignores a record that is not an object', () => {
    localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, '"mesh"');
    expect(readNavigationSnapshot()).toBeNull();
  });

  it('ignores a record written by another version', () => {
    localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, JSON.stringify({
      ...RECORD,
      version: NAVIGATION_SNAPSHOT_VERSION + 1,
    }));
    expect(readNavigationSnapshot()).toBeNull();
  });

  it('ignores a record whose View Mode is not one of the four modes', () => {
    localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, JSON.stringify({
      ...RECORD,
      viewMode: 'filtered',
    }));
    expect(readNavigationSnapshot()).toBeNull();
  });

  it('ignores a record whose ids are not ids', () => {
    localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, JSON.stringify({
      ...RECORD,
      selectedMeshId: 'two',
    }));
    expect(readNavigationSnapshot()).toBeNull();
  });

  it('keeps the record when the grid mode is missing and falls back to the neutral target', () => {
    const { gridMode: _gridMode, ...withoutGridMode } = RECORD;
    localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, JSON.stringify(withoutGridMode));
    expect(readNavigationSnapshot()).toMatchObject({
      viewMode: 'mesh',
      gridMode: 'all',
      selectedMeshId: 2,
      activeNodeId: 30,
    });
  });

  it('reads a record with no Mesh and no Agent Node', () => {
    localStorage.setItem(NAVIGATION_SNAPSHOT_STORAGE_KEY, JSON.stringify({
      version: NAVIGATION_SNAPSHOT_VERSION,
      viewMode: 'all',
      gridMode: 'all',
      selectedMeshId: null,
      selectedMeshName: null,
      activeNodeId: null,
      activeNodeName: null,
    }));
    expect(readNavigationSnapshot()).toMatchObject({
      viewMode: 'all',
      selectedMeshId: null,
      activeNodeId: null,
    });
  });

  it('survives unavailable storage', () => {
    const getItem = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('storage unavailable');
    });
    expect(readNavigationSnapshot()).toBeNull();
    getItem.mockRestore();

    const setItem = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('storage unavailable');
    });
    expect(() => writeNavigationSnapshot(RECORD)).not.toThrow();
    setItem.mockRestore();
  });
});
