#!/usr/bin/env node
// Issue #1568 - desktop bundle size budget gate.
//
// The desktop build's initial JS chunk (dist/assets/index-*.js) is the
// single largest byte-cost on first paint. Without an enforced budget,
// Vite's chunk-size warning is just noise and regressions slip in
// unnoticed. This script reads the build's dist/ output, computes the
// gzipped size of the entry chunk and its companion CSS, compares them
// against the agreed budget in `scripts/bundle-budget.json`, and exits
// non-zero on a breach so CI fails loudly.
//
// Run via `npm run check:bundle` (or as part of `scripts/check.ps1
// all-ts`). The script is intentionally dependency-free — it uses only
// Node built-ins (`fs`, `path`, `zlib`) — so it can run in CI without an
// extra `npm install` step and the verification surface stays small.
//
// Exit codes:
//   0 = within budget
//   1 = one or more assets over budget, or dist/ missing
//   2 = config / CLI argument error
//
// JSON mode (`--json`) prints a machine-readable report suitable for
// posting as a PR comment or archiving in a build artifact.

import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { gzipSync } from 'node:zlib';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const defaultBudgetPath = join(root, 'scripts', 'bundle-budget.json');
const defaultDistDir = join(root, 'dist');

function parseArgs(argv) {
  const out = { budget: defaultBudgetPath, dist: defaultDistDir, json: false };
  for (let i = 2; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--budget') out.budget = resolve(argv[++i]);
    else if (arg === '--dist') out.dist = resolve(argv[++i]);
    else if (arg === '--json') out.json = true;
    else if (arg === '--help' || arg === '-h') {
      printHelp();
      process.exit(0);
    } else {
      console.error(`Unknown argument: ${arg}`);
      printHelp();
      process.exit(2);
    }
  }
  return out;
}

function printHelp() {
  console.log(`Usage: node scripts/check-bundle-size.mjs [options]

Options:
  --budget <path>   Path to budget JSON (default: scripts/bundle-budget.json)
  --dist <path>     Path to Vite output (default: dist/)
  --json            Emit a JSON report on stdout and exit non-zero on
                    breach; suitable for piping into a CI artifact
  -h, --help        Show this help`);
}

// The script accepts either a glob (e.g. "index-*.js") or a literal
// filename (e.g. "index.js") and resolves to the single file in dist/
// (or its `assets/` subdirectory) matching the pattern. Throws if the
// dist is missing or ambiguous. Vite emits JS/CSS into `assets/` for
// production builds; we look there first and fall back to the root
// for callers that point the script at a custom layout.
function findAsset(distDir, pattern) {
  if (!existsSync(distDir)) {
    throw new Error(`dist directory not found: ${distDir}\nRun \`npm run build\` first.`);
  }
  const candidates = [distDir, join(distDir, 'assets')].filter((d) => existsSync(d));
  const matches = [];
  for (const dir of candidates) {
    for (const f of readdirSync(dir)) {
      const full = join(dir, f);
      if (!statSync(full).isFile()) continue;
      if (matchGlob(pattern, f)) matches.push(full);
    }
  }
  if (matches.length === 0) {
    throw new Error(`No asset matching "${pattern}" in ${distDir} or ${distDir}/assets`);
  }
  if (matches.length > 1) {
    throw new Error(`Multiple assets matched "${pattern}": ${matches.map((p) => p.split(/[\\/]/).pop()).join(', ')}\nUse a more specific pattern in the budget file.`);
  }
  return matches[0];
}

// Tiny glob matcher sufficient for the patterns this script uses
// ("index-*.js", "*.css"). Avoids pulling in minimatch / glob for one
// pattern match.
function matchGlob(pattern, name) {
  if (pattern === name) return true;
  const regex = new RegExp('^' + pattern.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[^/]*') + '$');
  return regex.test(name);
}

function gzipSize(bytes) {
  return gzipSync(bytes, { level: 9 }).byteLength;
}

function listAssets(distDir) {
  if (!existsSync(distDir)) return [];
  const dirs = [distDir, join(distDir, 'assets')].filter((d) => existsSync(d));
  const seen = new Set();
  const out = [];
  for (const dir of dirs) {
    for (const f of readdirSync(dir)) {
      if (seen.has(f)) continue;
      if (!statSync(join(dir, f)).isFile()) continue;
      if (!/\.(js|css)$/.test(f)) continue;
      seen.add(f);
      out.push(f);
    }
  }
  return out;
}

function evaluate(budget, distDir) {
  const assets = listAssets(distDir);
  const results = [];
  const allChecks = [...budget.checks];
  for (const check of allChecks) {
    const assetPath = findAsset(distDir, check.pattern);
    const raw = readFileSync(assetPath);
    const rawSize = raw.byteLength;
    const gzSize = gzipSize(raw);
    const overRaw = check.maxRaw != null && rawSize > check.maxRaw;
    const overGzip = check.maxGzip != null && gzSize > check.maxGzip;
    results.push({
      pattern: check.pattern,
      file: assetPath.split(/[\\/]/).pop(),
      label: check.label,
      rawBytes: rawSize,
      gzipBytes: gzSize,
      maxRaw: check.maxRaw ?? null,
      maxGzip: check.maxGzip ?? null,
      overBudget: overRaw || overGzip,
      deltaRaw: check.maxRaw != null ? rawSize - check.maxRaw : null,
      deltaGzip: check.maxGzip != null ? gzSize - check.maxGzip : null,
    });
  }
  const totalGzip = results.reduce((sum, r) => sum + r.gzipBytes, 0);
  const totalRaw = results.reduce((sum, r) => sum + r.rawBytes, 0);
  return { results, totalGzip, totalRaw, assetCount: assets.length };
}

function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} kB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

function printReport(report) {
  console.log('Bundle size report');
  console.log('==================');
  for (const r of report.results) {
    const status = r.overBudget ? 'OVER' : 'ok  ';
    const rawBudget = r.maxRaw != null ? ` (limit ${fmtBytes(r.maxRaw)})` : '';
    const gzipBudget = r.maxGzip != null ? ` (limit ${fmtBytes(r.maxGzip)})` : '';
    console.log(
      `[${status}] ${r.label.padEnd(36)} ` +
      `raw=${fmtBytes(r.rawBytes).padStart(8)}${rawBudget.padEnd(20)} ` +
      `gzip=${fmtBytes(r.gzipBytes).padStart(8)}${gzipBudget}`,
    );
  }
  console.log('-'.repeat(72));
  console.log(`Total initial JS/CSS (sum of checked assets): raw=${fmtBytes(report.totalRaw)}, gzip=${fmtBytes(report.totalGzip)}`);
  console.log(`(Dist contained ${report.assetCount} .js/.css files; only budget-listed assets are checked.)`);
}

function main() {
  const args = parseArgs(process.argv);
  let budget;
  try {
    budget = JSON.parse(readFileSync(args.budget, 'utf8'));
  } catch (e) {
    console.error(`Failed to read budget file ${args.budget}: ${e.message}`);
    process.exit(2);
  }
  if (!budget.checks || !Array.isArray(budget.checks) || budget.checks.length === 0) {
    console.error('Budget file must define a non-empty `checks` array.');
    process.exit(2);
  }
  let report;
  try {
    report = evaluate(budget, args.dist);
  } catch (e) {
    console.error(e.message);
    process.exit(2);
  }
  if (args.json) {
    process.stdout.write(JSON.stringify(report, null, 2) + '\n');
  } else {
    printReport(report);
  }
  const over = report.results.filter((r) => r.overBudget);
  if (over.length > 0) {
    if (!args.json) {
      console.error('\nBundle budget exceeded:');
      for (const r of over) {
        const where = [];
        if (r.deltaRaw != null && r.deltaRaw > 0) where.push(`raw +${fmtBytes(r.deltaRaw)}`);
        if (r.deltaGzip != null && r.deltaGzip > 0) where.push(`gzip +${fmtBytes(r.deltaGzip)}`);
        console.error(`  - ${r.label} (${r.file}) — ${where.join(', ')}`);
      }
      console.error('\nIssue #1568: keep the desktop initial bundle within the documented budget so first paint stays under 1 MB transfer.');
    }
    process.exit(1);
  }
  if (!args.json) {
    console.log('\nBundle budget OK.');
  }
  process.exit(0);
}

main();
