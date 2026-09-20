#!/usr/bin/env node
// Brand asset generation.
//
// The brand sources are SVG under `docs/brand/`. Everything the app, the
// README and the installers actually consume is produced from them by this
// script, so there is exactly one copy of each piece of artwork to edit:
//
//   docs/brand/b3-relay-icon.svg       -> the two SVG copies below
//                                      -> every small PNG (favicon, touch icons)
//   docs/brand/b3-relay-lockup-*.svg   -> the README wordmark rasters
//
// The SVG copies exist because Vite and the mobile SPA each need the mark at a
// path they can serve; they are verbatim copies plus a generated-file banner,
// and `tests/unit/brand-wordmark.test.tsx` fails if they drift from the source.
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
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** The single source for the compact mark. Edit this, not its copies. */
const COMPACT = 'docs/brand/b3-relay-icon.svg';
const LOCKUP_ON_DARK = 'docs/brand/b3-relay-lockup-dark.svg';
const LOCKUP_ON_LIGHT = 'docs/brand/b3-relay-lockup-light.svg';

/** Verbatim copies of COMPACT, for the two bundles that need a served path. */
const SVG_COPIES = ['src/assets/logo.svg', 'mobile/public/favicon.svg'];

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
  console.log(`SVG copies from ${COMPACT}:`);
  writeSvgCopies();
  console.log('\nRasters:');
  await rasterise();
  console.log(`\nDone. Repo root: ${relative(process.cwd(), repoRoot) || '.'}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
