import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// While an infinite animation runs, Chromium keeps producing frames instead of
// going idle. An `opacity` or `transform` animation is applied by the
// compositor, so those frames only re-composite layers. Other properties
// (`border-color`, `background-color`, `box-shadow`, ...) are applied on the
// main thread and repaint the animated element's box each frame. The NodeCard
// attention pulse animated `border-color` on a card that fills a grid cell,
// which cost about 30% of a core for four cards in a 4K window (measured with
// the mock UI), against about 10% for 24 opacity pulses.
const STYLESHEETS = ['../../src/App.css', '../../src/mobile/styles.css'];
const COMPOSITOR_PROPERTIES = new Set(['opacity', 'transform']);

function infiniteAnimationNames(css: string): Set<string> {
  const names = new Set<string>();
  for (const [, body] of css.matchAll(/\{([^{}]*)\}/g)) {
    const shorthand = body.match(/(?:^|;|\s)animation\s*:\s*([^;]+)/);
    if (shorthand && /\binfinite\b/.test(shorthand[1])) {
      names.add(shorthand[1].trim().split(/\s+/)[0]);
    }
    const name = body.match(/animation-name\s*:\s*([\w-]+)/);
    if (name && /animation-iteration-count\s*:\s*infinite/.test(body)) names.add(name[1]);
  }
  return names;
}

function keyframeProperties(css: string, name: string): string[] {
  const start = css.search(new RegExp(`@keyframes\\s+${name}\\s*\\{`));
  expect(start, `@keyframes ${name} must be defined`).toBeGreaterThanOrEqual(0);
  let depth = 0;
  let end = css.indexOf('{', start);
  for (; end < css.length; end++) {
    if (css[end] === '{') depth++;
    if (css[end] === '}' && --depth === 0) break;
  }
  const block = css.slice(css.indexOf('{', start) + 1, end);
  return [...block.matchAll(/([\w-]+)\s*:/g)].map(([, property]) => property);
}

describe('infinite CSS animations', () => {
  it.each(STYLESHEETS)('in %s animate only compositor properties', (path) => {
    const css = readFileSync(resolve(__dirname, path), 'utf8');
    const names = infiniteAnimationNames(css);
    expect(names.size).toBeGreaterThan(0);
    for (const name of names) {
      const painted = keyframeProperties(css, name).filter(property => !COMPOSITOR_PROPERTIES.has(property));
      expect(painted, `@keyframes ${name} repaints on every frame`).toEqual([]);
    }
  });
});
