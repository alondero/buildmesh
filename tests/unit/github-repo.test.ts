import { describe, it, expect } from 'vitest';
import { joinDisplayPath, repoNameFromInput } from '../../src/lib/githubRepo';

// `repoNameFromInput` mirrors the backend `parse_clone_input` rules
// (`src-tauri/src/services/github/sync.rs`); it drives the clone form's
// destination preview and enable/disable gate. Its Rust counterpart is pinned
// by `parse_clone_input_*` tests — keep the accepted shapes in step.
describe('repoNameFromInput', () => {
  it('parses the bare owner/repo shorthand', () => {
    expect(repoNameFromInput('alondero/buildmesh')).toBe('buildmesh');
    expect(repoNameFromInput('  alondero/buildmesh  ')).toBe('buildmesh');
    expect(repoNameFromInput('alondero/buildmesh.git')).toBe('buildmesh');
  });

  it('parses github.com HTTPS URLs, with or without .git or a trailing slash', () => {
    expect(repoNameFromInput('https://github.com/alondero/buildmesh')).toBe('buildmesh');
    expect(repoNameFromInput('https://github.com/alondero/buildmesh.git')).toBe('buildmesh');
    expect(repoNameFromInput('https://github.com/alondero/buildmesh/')).toBe('buildmesh');
  });

  it('parses SSH forms', () => {
    expect(repoNameFromInput('git@github.com:alondero/buildmesh.git')).toBe('buildmesh');
    expect(repoNameFromInput('ssh://git@github.com/alondero/buildmesh')).toBe('buildmesh');
  });

  it('rejects non-github hosts, incomplete refs, and local paths', () => {
    expect(repoNameFromInput('')).toBeNull();
    expect(repoNameFromInput('   ')).toBeNull();
    expect(repoNameFromInput('buildmesh')).toBeNull();
    expect(repoNameFromInput('https://gitlab.com/foo/bar')).toBeNull();
    expect(repoNameFromInput('owner/repo/extra')).toBeNull();
    expect(repoNameFromInput('owner /repo')).toBeNull();
    expect(repoNameFromInput('C:/repos')).toBeNull();
  });

  // Parity with the backend: only the exact github.com forms the backend's
  // `parse_clone_input` takes are accepted, so this gate can never enable
  // Clone for input the backend would reject.
  it('rejects github.com URLs in forms the backend does not accept', () => {
    expect(repoNameFromInput('http://github.com/alondero/buildmesh')).toBeNull();
    expect(repoNameFromInput('git://github.com/alondero/buildmesh')).toBeNull();
    expect(repoNameFromInput('HTTPS://github.com/alondero/buildmesh')).toBeNull();
    expect(repoNameFromInput('ssh://git@gitlab.com/alondero/buildmesh')).toBeNull();
    expect(repoNameFromInput('git@gitlab.com:alondero/buildmesh')).toBeNull();
    expect(repoNameFromInput('https://github.com/')).toBeNull();
  });

  it('rejects `.`/`..` repo names, matching the backend traversal guard', () => {
    expect(repoNameFromInput('owner/..')).toBeNull();
    expect(repoNameFromInput('https://github.com/owner/..')).toBeNull();
  });
});

describe('joinDisplayPath', () => {
  it('joins with whichever separator the parent already uses', () => {
    expect(joinDisplayPath('D:\\repos', 'buildmesh')).toBe('D:\\repos\\buildmesh');
    expect(joinDisplayPath('/home/me/repos', 'buildmesh')).toBe('/home/me/repos/buildmesh');
  });

  it('does not double up separators', () => {
    expect(joinDisplayPath('D:\\repos\\', 'buildmesh')).toBe('D:\\repos\\buildmesh');
    expect(joinDisplayPath('/home/me/repos/', 'buildmesh')).toBe('/home/me/repos/buildmesh');
  });
});
