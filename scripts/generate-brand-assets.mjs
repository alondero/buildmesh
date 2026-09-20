#!/usr/bin/env node
// Brand asset generation.
//
// The committed brand sources are SVG (`docs/brand/*.svg`, `src/assets/logo.svg`).
// The app, the README and the installers need raster PNGs, so this script
// rasterises them with Playwright's Chromium — already a devDependency — at
// exact pixel sizes with a transparent background.
//
// Run:
//   npm run brand:build
//
// The wordmark lockups set their text in Geist, and this script loads the
// same Google Fonts request `index.html` uses so the rasterised text matches
// what the app renders. Rasterising therefore needs network access; the
// resulting PNGs are committed so normal builds never depend on it.
//
// App icons (`.ico` / `.icns` / the store logos) are NOT produced here —
// `npx tauri icon` owns those, from `src-tauri/app-icon.svg`.

import { chromium } from '@playwright/test';
import { readFileSync, mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const MARK = 'src/assets/logo.svg';
const LOCKUP_ON_DARK = 'docs/brand/b3-relay-lockup-dark.svg';
const LOCKUP_ON_LIGHT = 'docs/brand/b3-relay-lockup-light.svg';

/** Each entry rasterises one source SVG to one PNG at an exact pixel size.
 *  `size` is [width, height] and must match the source viewBox aspect ratio. */
const JOBS = [
  { src: MARK, out: 'src/assets/logo.png', size: [256, 256] },
  { src: MARK, out: 'src/assets/apple-touch-icon.png', size: [180, 180] },
  { src: MARK, out: 'mobile/public/icon-192.png', size: [192, 192] },
  { src: MARK, out: 'mobile/public/icon-512.png', size: [512, 512] },
  { src: MARK, out: 'mobile/public/apple-touch-icon.png', size: [180, 180] },
  { src: LOCKUP_ON_DARK, out: 'src/assets/wordmark-on-dark.png', size: [1440, 480] },
  { src: LOCKUP_ON_LIGHT, out: 'src/assets/wordmark-on-light.png', size: [1440, 480] },
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

async function main() {
  const browser = await chromium.launch();
  const page = await browser.newPage({ deviceScaleFactor: 1 });

  for (const job of JOBS) {
    const srcPath = resolve(repoRoot, job.src);
    const outPath = resolve(repoRoot, job.out);
    const svgMarkup = readFileSync(srcPath, 'utf8');

    await page.setContent(pageFor(svgMarkup, job.size), { waitUntil: 'load' });
    await page.evaluate(() => document.fonts.ready);
    // `fonts.ready` resolves once the request settles; the lockups need the
    // 700/800 weights actually rasterised before the screenshot is honest.
    await page.evaluate(() => document.fonts.load('700 33px Geist'));

    mkdirSync(dirname(outPath), { recursive: true });
    await page.locator('#shot').screenshot({ path: outPath, omitBackground: true });
    console.log(`  ${job.out}  ${job.size[0]}x${job.size[1]}  <- ${job.src}`);
  }

  await browser.close();
  console.log(`\nGenerated ${JOBS.length} raster assets.`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
