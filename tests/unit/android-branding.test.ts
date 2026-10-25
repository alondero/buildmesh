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

  it.each([
    ['transform', 'translate(4 0)'], ['opacity', '0.5'],
    ['fill-opacity', '0.5'], ['stroke-opacity', '0.5'],
    ['clip-path', 'url(#clip)'], ['mask', 'url(#mask)'],
    ['style', 'fill: #ffffff'], ['fill', 'url(#gradient)'], ['stroke', 'url(#gradient)'],
  ])('rejects unsupported SVG paint %s', (attribute, value) => {
    const source = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96"><path d="M0 0L1 1" ${attribute}="${value}"/></svg>`;
    expect(() => androidVector(source, 'fixture.svg')).toThrow(/Unsupported Relay SVG/);
  });

  it('selects the monochrome plate by geometry when a node precedes it', () => {
    const source = read('docs/brand/b3-relay-icon.svg').replace(
      '<rect x="2"', '<rect x="0" y="0" width="4" height="4" rx="1" fill="#00d4ff"/><rect x="2"',
    );
    const vector = androidVector(source, 'fixture.svg', true);
    expect(vector).not.toContain('M28,2h40a26,26');
    expect(vector).toContain('android:pathData="M1,0h2a1,1');
  });

  it('preserves an explicitly absent stroke as transparent', () => {
    const source = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96"><path d="M0 0L1 1" fill="#00d4ff" stroke="none"/></svg>';
    expect(androidVector(source, 'fixture.svg')).toContain('android:strokeColor="@android:color/transparent"');
  });
});
