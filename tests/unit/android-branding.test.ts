import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { androidVector } from '../../scripts/generate-brand-assets.mjs';

const read = (path: string) => readFileSync(resolve(__dirname, '../..', path), 'utf8');

describe('Android brand sources', () => {
  it.each([
    ['docs/brand/b3-relay-icon.svg', 'android/app/src/main/res/drawable/ic_buildmesh.xml'],
    ['docs/brand/b3-relay-mark.svg', 'android/app/src/main/res/drawable/relay_mark.xml'],
    ['docs/brand/b3-relay-icon.svg', 'android/app/src/main/res/drawable/relay_monochrome.xml', true],
  ])('%s generates the shipped native vector %s', (source, target, monochrome) => {
    expect(read(target)).toBe(androidVector(read(source), source, monochrome));
  });

  it('keeps themed launcher art transparent around the mesh silhouette', () => {
    const vector = read('android/app/src/main/res/drawable/relay_monochrome.xml');
    expect(vector).not.toContain('M28,2h40a26,26');
    expect(vector).not.toMatch(/#(?:0a0a0e|00d4ff|22c55e|8b5cf6)/);
    expect(vector.match(/<path /g)).toHaveLength(9);
  });

  it('mirrors desktop dark palette tokens', () => {
    const css = read('src/App.css').split('/* Light theme override')[0];
    const kotlin = read('android/app/src/main/java/dev/buildmesh/remote/Theme.kt');
    for (const [native, desktop] of [
      ['background', 'bg-base'], ['surface', 'bg-surface'], ['overlay', 'bg-overlay'],
      ['card', 'bg-card'], ['cardHover', 'bg-card-hover'], ['input', 'bg-input'],
      ['selection', 'bg-selection'], ['text', 'text-primary'], ['secondary', 'text-secondary'],
      ['muted', 'text-muted'], ['accent', 'accent-cyan'], ['violet', 'accent-violet'],
      ['green', 'accent-green'], ['amber', 'accent-amber'], ['red', 'accent-red'],
    ]) {
      const value = css.match(new RegExp(`--color-${desktop}:\\s*#([0-9a-f]{6});`))?.[1];
      expect(value, desktop).toBeDefined();
      expect(kotlin, native).toContain(`val ${native} = Color(0xff${value})`);
    }
    const colours = read('android/app/src/main/res/values/colors.xml');
    expect(colours).toContain('<color name="buildmesh_background">#0a0a0e</color>');
    expect(colours).toContain('<color name="buildmesh_accent">#00d4ff</color>');
    for (const [native, desktop] of [
      ['borderStrong', 'border-strong'], ['borderDefault', 'border-default'], ['borderSubtle', 'border-subtle'],
    ]) {
      const alpha = css.match(new RegExp(`--color-${desktop}:\\s*rgba\\(255, 255, 255, ([0-9.]+)\\);`))?.[1];
      expect(alpha, desktop).toBeDefined();
      expect(kotlin).toContain(`val ${native} = Color.White.copy(alpha = ${alpha}f)`);
    }
  });

  it('preserves rounded plates, split wires and coloured nodes in native vectors', () => {
    const vector = read('android/app/src/main/res/drawable/relay_mark.xml');
    expect(vector).toContain('android:pathData="M29,4h38a25,25');
    expect(vector).toContain('android:strokeColor="#00d4ff" android:strokeWidth="5.5" android:strokeLineCap="round"');
    expect(vector).toContain('android:strokeColor="#22c55e" android:strokeWidth="5.5" android:strokeLineCap="round"');
    expect(vector).toContain('android:fillColor="#8b5cf6" android:pathData="M40.5,53a7.5,7.5');
    expect(vector.slice(4, vector.indexOf('-->'))).not.toContain('--');
  });
});
