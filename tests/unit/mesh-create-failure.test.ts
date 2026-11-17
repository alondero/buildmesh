import { describe, it, expect } from 'vitest';
import {
  classifyMeshCreateFailure,
  meshCreateFailureDetail,
} from '../../src/lib/meshCreateFailure';

describe('classifyMeshCreateFailure', () => {
  // The message is the backend's own text. The classifier does not rewrite
  // it, so a distinctive rejection still reaches the dialog (issue #2794).
  it('keeps the backend message verbatim', () => {
    const failure = classifyMeshCreateFailure(
      'Access denied creating repository workspace',
      '/repos/app'
    );
    expect(failure.message).toBe('Access denied creating repository workspace');
  });

  it('recognises a missing folder and offers another folder', () => {
    for (const message of [
      'The system cannot find the path specified.',
      'No such file or directory (os error 2)',
      'ENOENT: no such file or directory',
      'path does not exist',
    ]) {
      const failure = classifyMeshCreateFailure(message);
      expect(failure.kind).toBe('missing-folder');
      expect(failure.recoveries).toEqual(['change-folder']);
    }
  });

  it('recognises a refused folder and offers another folder', () => {
    for (const message of [
      'Access denied creating repository workspace',
      'Access is denied. (os error 5)',
      'Permission denied (os error 13)',
      'Operation not permitted',
    ]) {
      const failure = classifyMeshCreateFailure(message);
      expect(failure.kind).toBe('access-denied');
      expect(failure.recoveries).toEqual(['change-folder']);
    }
  });

  it('recognises a transient failure and offers a retry', () => {
    for (const message of [
      'database is locked',
      'database table is locked: meshes',
      'SQLITE_BUSY: database is locked',
      'The request timed out',
      'Service unavailable, please try again',
    ]) {
      const failure = classifyMeshCreateFailure(message);
      expect(failure.kind).toBe('transient');
      expect(failure.recoveries).toEqual(['retry']);
    }
  });

  it('keeps both safe choices when the cause does not match a known one', () => {
    const failure = classifyMeshCreateFailure('the backend said something unexpected');
    expect(failure.kind).toBe('unknown');
    expect(failure.recoveries).toEqual(['retry', 'change-folder']);
    // The real message is retained for an unclassified cause.
    expect(failure.message).toBe('the backend said something unexpected');
  });

  it('names the next step in one short sentence', () => {
    expect(classifyMeshCreateFailure('os error 2').hint).toMatch(/choose the repository folder again/i);
    expect(classifyMeshCreateFailure('Access is denied').hint).toMatch(/fix access|choose a different folder/i);
    expect(classifyMeshCreateFailure('database is locked').hint).toMatch(/temporary/i);
    expect(classifyMeshCreateFailure('something odd').hint).toMatch(/try again/i);
  });

  it('names the operation, the folder and the cause in the diagnostic block', () => {
    const failure = classifyMeshCreateFailure('Access denied', 'D:\\repos\\app');
    expect(failure.detail).toContain('Operation: Open repository folder');
    expect(failure.detail).toContain('Folder: D:\\repos\\app');
    expect(failure.detail).toContain('Cause: Access denied');
  });

  it('omits the folder from the diagnostic block when there is none', () => {
    expect(meshCreateFailureDetail('boom', null)).toBe(
      'Operation: Open repository folder\nCause: boom'
    );
  });

  it('treats an empty message as unclassified rather than crashing', () => {
    const failure = classifyMeshCreateFailure('   ');
    expect(failure.kind).toBe('unknown');
    expect(failure.message).toBe('');
    expect(failure.recoveries).toEqual(['retry', 'change-folder']);
  });
});
