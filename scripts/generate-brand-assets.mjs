#!/usr/bin/env node
// Brand asset generation.
//
// The brand artwork sources are SVG under `docs/brand/`. This script produces
// the copies below; native platform wiring and bundled fonts are maintained
// separately, as listed in docs/brand/README.md:
//
//   docs/brand/b3-relay-icon.svg       -> the two SVG copies below
//                                      -> every small PNG (favicon, touch icons)
//   docs/brand/b3-relay-lockup-*.svg   -> the README wordmark rasters
//   docs/brand/b3-relay-{icon,mark}.svg -> Android vectors
//
// The SVG copies exist because Vite and the mobile SPA each need the mark at a
// path they can serve; regenerate them here rather than editing the copies.
//
// Run:
//   npm run brand:build
//
// Rasterising uses Playwright's Chromium (already a devDependency) and loads
// the same Google Fonts request `index.html` uses, so the lockup text matches
// what the app renders. It therefore needs network access; the resulting PNGs
// are committed so ordinary builds never depend on it.
//
// App icons (.ico / .icns / store logos) are NOT produced here — `npx tauri
// icon src-tauri/app-icon.svg` owns those.

import { chromium } from '@playwright/test';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, resolve, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { JSDOM } from 'jsdom';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** The single source for the compact mark. Edit this, not its copies. */
const COMPACT = 'docs/brand/b3-relay-icon.svg';
const LOCKUP_ON_DARK = 'docs/brand/b3-relay-lockup-dark.svg';
const LOCKUP_ON_LIGHT = 'docs/brand/b3-relay-lockup-light.svg';

/** Verbatim copies of COMPACT, for the two bundles that need a served path. */
const SVG_COPIES = ['src/assets/logo.svg', 'mobile/public/favicon.svg'];

const ANDROID_VECTORS = [
  { src: COMPACT, out: 'android/app/src/main/res/drawable/ic_buildmesh.xml' },
  { src: 'docs/brand/b3-relay-mark.svg', out: 'android/app/src/main/res/drawable/relay_mark.xml' },
  { src: COMPACT, out: 'android/app/src/main/res/drawable/relay_monochrome.xml', monochrome: true },
];

const PAINT_ATTRIBUTES = ['fill', 'stroke', 'stroke-width', 'stroke-linecap'];
const ANDROID_SVG_ATTRIBUTES = new Map(Object.entries({
  svg: ['xmlns', 'viewBox', 'width', 'height', 'role', 'aria-labelledby', ...PAINT_ATTRIBUTES],
  title: ['id'], g: PAINT_ATTRIBUTES,
  path: ['d', ...PAINT_ATTRIBUTES],
  circle: ['cx', 'cy', 'r', ...PAINT_ATTRIBUTES],
  rect: ['x', 'y', 'width', 'height', 'rx', ...PAINT_ATTRIBUTES],
}).map(([name, attributes]) => [name, new Set(attributes)]));

/** Untransformed paths, circles and rounded rects with solid six-digit hex paint
 *  or none. Reject other SVG paint features rather than silently changing art. */
export function androidVector(svgMarkup, source, monochrome = false) {
  const dom = new JSDOM(svgMarkup, { contentType: 'image/svg+xml' });
  try {
    const svg = dom.window.document.documentElement;
    const area = element => Number(element.getAttribute('width')) * Number(element.getAttribute('height'));
    const plate = monochrome ? [...svg.querySelectorAll('rect[rx]')].sort((a, b) => area(b) - area(a))[0] : null;
    const [, , width, height] = svg.getAttribute('viewBox').split(/\s+/).map(Number);
    const paths = [];
    function visit(element, inherited = { fill: 'none' }) {
      const allowed = ANDROID_SVG_ATTRIBUTES.get(element.localName);
      if (!allowed) throw new Error(`Unsupported Relay SVG element: ${element.localName}`);
      for (const attribute of element.attributes) {
        if (!allowed.has(attribute.name)) throw new Error(`Unsupported Relay SVG attribute: ${attribute.name}`);
        if (['fill', 'stroke'].includes(attribute.name) && attribute.value !== 'none' && !/^#[0-9a-f]{6}$/i.test(attribute.value)) {
          throw new Error(`Unsupported Relay SVG paint: ${attribute.value}`);
        }
      }
      if (element === plate) return;
      const attrs = { ...inherited, ...Object.fromEntries([...element.attributes].map(attr => [attr.name, attr.value])) };
      const number = name => Number(attrs[name] ?? 0);
      let data;
      if (element.localName === 'path') data = attrs.d;
      else if (element.localName === 'circle') {
        const x = number('cx'), y = number('cy'), r = number('r');
        data = `M${x - r},${y}a${r},${r} 0,1 0,${2 * r},0a${r},${r} 0,1 0,${-2 * r},0`;
      } else if (element.localName === 'rect') {
        const x = number('x'), y = number('y'), w = number('width'), h = number('height'), r = number('rx');
        data = `M${x + r},${y}h${w - 2 * r}a${r},${r} 0,0 1,${r},${r}v${h - 2 * r}a${r},${r} 0,0 1,${-r},${r}h${2 * r - w}a${r},${r} 0,0 1,${-r},${-r}v${2 * r - h}a${r},${r} 0,0 1,${r},${-r}z`;
      }
      if (data) {
        const fields = [`android:fillColor="${attrs.fill === 'none' ? '@android:color/transparent' : monochrome ? '#ffffff' : attrs.fill}"`, `android:pathData="${data}"`];
        for (const [svgAttr, androidAttr] of [['stroke', 'strokeColor'], ['stroke-width', 'strokeWidth'], ['stroke-linecap', 'strokeLineCap']]) {
          let value = attrs[svgAttr];
          if (value && svgAttr === 'stroke') value = value === 'none' ? '@android:color/transparent' : monochrome ? '#ffffff' : value;
          if (value) fields.push(`android:${androidAttr}="${value}"`);
        }
        paths.push(`    <path ${fields.join(' ')} />`);
      }
      for (const child of element.children) visit(child, attrs);
    }
    visit(svg);
    return `<!-- Generated from ${source} by npm run brand:build; do not edit by hand. -->\n<vector xmlns:android="http://schemas.android.com/apk/res/android" android:width="${width}dp" android:height="${height}dp" android:viewportWidth="${width}" android:viewportHeight="${height}">\n${paths.join('\n')}\n</vector>\n`;
  } finally {
    dom.window.close();
  }
}

export function writeAndroidVectors() {
  for (const { src, out, monochrome } of ANDROID_VECTORS) {
    writeFileSync(resolve(repoRoot, out), androidVector(readFileSync(resolve(repoRoot, src), 'utf8'), src, monochrome));
    console.log(`  ${out}  <- ${src}`);
  }
}

export const GENERATED_BANNER =
  `<!-- Generated from ${COMPACT} by \`npm run brand:build\` — do not edit by hand. -->`;

/** Each job rasterises one source SVG to one PNG. `size` must match the
 *  source viewBox aspect ratio. Rasters are sized for their real display use:
 *  the wordmark is a README hero at width="420", so 840px is exact retina. */
const JOBS = [
  { src: COMPACT, out: 'src/assets/logo.png', size: [256, 256] },
  { src: COMPACT, out: 'src/assets/apple-touch-icon.png', size: [180, 180] },
  { src: COMPACT, out: 'mobile/public/icon-192.png', size: [192, 192] },
  { src: COMPACT, out: 'mobile/public/icon-512.png', size: [512, 512] },
  { src: COMPACT, out: 'mobile/public/apple-touch-icon.png', size: [180, 180] },
  { src: LOCKUP_ON_DARK, out: 'docs/brand/wordmark-on-dark.png', size: [840, 240] },
  { src: LOCKUP_ON_LIGHT, out: 'docs/brand/wordmark-on-light.png', size: [840, 240] },
];

const FONTS =
  'https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600;700;800&display=swap';

function pageFor(svgMarkup, [width, height]) {
  return `<!doctype html>
<html><head><meta charset="utf-8" />
<link rel="preconnect" href="https://fonts.googleapis.com" />
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
<link href="${FONTS}" rel="stylesheet" />
<style>
  html, body { margin: 0; padding: 0; background: transparent; }
  #shot { width: ${width}px; height: ${height}px; }
  #shot > svg { display: block; width: 100%; height: 100%; }
</style>
</head><body><div id="shot">${svgMarkup}</div></body></html>`;
}

/** Write the canonical mark to each served path, with a do-not-edit banner. */
function writeSvgCopies() {
  const source = readFileSync(resolve(repoRoot, COMPACT), 'utf8');
  for (const target of SVG_COPIES) {
    const outPath = resolve(repoRoot, target);
    mkdirSync(dirname(outPath), { recursive: true });
    writeFileSync(outPath, `${GENERATED_BANNER}\n${source}`);
    console.log(`  ${target}  <- ${COMPACT}`);
  }
}

async function rasterise() {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ deviceScaleFactor: 1 });

    for (const job of JOBS) {
      const svgMarkup = readFileSync(resolve(repoRoot, job.src), 'utf8');
      await page.setContent(pageFor(svgMarkup, job.size), { waitUntil: 'load' });
      await page.evaluate(() => document.fonts.ready);
      // The lockups set their wordmark at weight 800. `fonts.ready` can settle
      // before that face is rasterised, which would bake a faux-bold 700, so
      // the weight is requested explicitly here (and 800 is in FONTS above).
      await page.evaluate(() => document.fonts.load('800 42px Geist'));

      const outPath = resolve(repoRoot, job.out);
      mkdirSync(dirname(outPath), { recursive: true });
      await page.locator('#shot').screenshot({ path: outPath, omitBackground: true });
      console.log(`  ${job.out}  ${job.size[0]}x${job.size[1]}  <- ${job.src}`);
    }
  } finally {
    // A failed job must not leak a Chromium process.
    await browser.close();
  }
}

async function main() {
  if (process.argv.includes('--android-only')) {
    writeAndroidVectors();
    return;
  }
  console.log(`SVG copies from ${COMPACT}:`);
  writeSvgCopies();
  console.log('\nAndroid vectors:');
  writeAndroidVectors();
  console.log('\nRasters:');
  await rasterise();
  console.log(`\nDone. Repo root: ${relative(process.cwd(), repoRoot) || '.'}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((err) => {
    console.error(err);
    process.exit(1);
  });
}
