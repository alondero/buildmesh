import { describe, it, expect } from 'vitest';
import {
  PROJECT_PRESETS,
  findPreset,
  resolvePreset,
} from '../../src/lib/projectPresets';

describe('PROJECT_PRESETS', () => {
  it('exposes the expected preset ids in priority order', () => {
    const ids = PROJECT_PRESETS.map((p) => p.id);
    expect(ids).toEqual([
      'rust',
      'node',
      'node-tauri',
      'jvm-gradle',
      'jvm-maven',
      'go',
      'python',
    ]);
  });

  it('every preset has non-empty build and run commands', () => {
    for (const preset of PROJECT_PRESETS) {
      expect(preset.build.trim()).not.toBe('');
      expect(preset.run.trim()).not.toBe('');
    }
  });
});

describe('findPreset', () => {
  it('returns a preset by id', () => {
    expect(findPreset('rust')?.build).toBe('cargo build');
  });

  it('returns undefined for unknown id', () => {
    expect(findPreset('cobol')).toBeUndefined();
  });
});

describe('resolvePreset', () => {
  it('returns the static preset for non-node ids regardless of nodeScripts', () => {
    const preset = resolvePreset('rust', {
      build: 'should-be-ignored',
      run: 'should-be-ignored',
      has_tauri_cli: true,
    });
    expect(preset?.build).toBe('cargo build');
    expect(preset?.run).toBe('cargo run');
  });

  it('returns the static node preset when no nodeScripts info is given', () => {
    const preset = resolvePreset('node');
    expect(preset?.build).toBe('npm run build');
    expect(preset?.run).toBe('npm run dev');
  });

  it('overlays detected node scripts onto the node preset', () => {
    const preset = resolvePreset('node', {
      build: 'build',
      run: 'serve',
      has_tauri_cli: false,
    });
    expect(preset?.build).toBe('npm run build');
    expect(preset?.run).toBe('npm run serve');
  });

  it('falls back to default scripts when detection finds none', () => {
    const preset = resolvePreset('node', {
      build: null,
      run: null,
      has_tauri_cli: false,
    });
    expect(preset?.build).toBe('npm run build');
    expect(preset?.run).toBe('npm run dev');
  });

  it('uses tauri fallbacks when node-tauri has no detected scripts', () => {
    const preset = resolvePreset('node-tauri', {
      build: null,
      run: null,
      has_tauri_cli: true,
    });
    expect(preset?.build).toBe('npm run tauri build');
    expect(preset?.run).toBe('npm run tauri dev');
  });

  it('returns undefined for unknown id', () => {
    expect(resolvePreset('cobol')).toBeUndefined();
  });
});
