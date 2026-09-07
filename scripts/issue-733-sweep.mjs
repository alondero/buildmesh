#!/usr/bin/env node
// Sweep script for issue #733 — bare `rounded` → `rounded-md`.
// Operates on src/components. Reads file list from argv.
//
// Conversion rules:
//   `rounded`        → `rounded-md`        (bare, NOT followed by `-`)
//   `rounded-r` etc. → `rounded-r-md` etc. (bare directional, NOT followed by another size suffix)
//
// Intentionally preserved:
//   `rounded-full`, `rounded-sm`, `rounded-md`, `rounded-lg`, `rounded-xl`,
//   `rounded-2xl`, `rounded-3xl`, `rounded-none`, `rounded-pill`
//   `rounded-r-md`, `rounded-l-sm`, etc. (already token-bound)
//
// Two intentionally decorative chips keep bare `rounded` and gain an
// `// allow-bare-rounded` comment to bypass the guard:
//   - src/components/Probe/WorktreeManagerTab.tsx (Badge, 9px status chip)
//   - src/components/Probe/GitPullRequestsTab.tsx (10px "can't merge" pill)

import { readFileSync, writeFileSync } from "node:fs";

const files = process.argv.slice(2);
if (files.length === 0) {
  console.error("Usage: node scripts/issue-733-sweep.mjs <file...>");
  process.exit(2);
}

// Lines whose bare `rounded` is intentionally smallest-radius and survives the sweep.
// We match a unique enough substring of the className. Keep these in sync with
// tests/unit/radii-audit.test.ts ALLOWED_BARE_ROUNDED.
const KEEP_FRAGMENTS = [
  "px-1 py-px rounded text-2xs", // WorktreeManagerTab Badge (was text-[9px] pre-#733 modernization)
  "px-2 py-1 text-2xs rounded bg-bg-card text-text-muted", // GitPullRequestsTab "can't merge" (was text-[10px])
];

// 1. Bare directional: `rounded-r`, `rounded-l`, `rounded-t`, `rounded-b`,
//    `rounded-tl`, `rounded-tr`, `rounded-bl`, `rounded-br` — convert to `rounded-{dir}-md`.
//    Negative lookahead excludes already-tokenised variants like `rounded-r-sm`.
const DIR_RE = /\brounded-(r|l|t|b|tl|tr|bl|br)\b(?![-](?:sm|md|lg|xl|2xl|3xl|none|pill|full)\b)/g;

// 2. Bare `rounded` — convert to `rounded-md`.
//    Negative lookahead `(?![-])` excludes anything like `rounded-full`, `rounded-r`, etc.
const BARE_RE = /\brounded\b(?![-])/g;

let totalReplaced = 0;
let totalFilesTouched = 0;

for (const file of files) {
  const original = readFileSync(file, "utf8");
  const lines = original.split("\n");
  let fileChanged = false;
  let fileReplaced = 0;

  const next = lines.map((line) => {
    if (KEEP_FRAGMENTS.some((frag) => line.includes(frag))) {
      // Leave the rounded unchanged; user adds // allow-bare-rounded separately if needed.
      return line;
    }
    let out = line;
    out = out.replace(DIR_RE, "rounded-$1-md");
    out = out.replace(BARE_RE, "rounded-md");
    if (out !== line) {
      fileChanged = true;
      fileReplaced += (line.match(DIR_RE) ?? []).length + (line.match(BARE_RE) ?? []).length;
    }
    return out;
  });

  if (fileChanged) {
    writeFileSync(file, next.join("\n"));
    totalFilesTouched += 1;
    totalReplaced += fileReplaced;
    console.log(`touched  ${file}  (${fileReplaced} replacements)`);
  } else {
    console.log(`skipped  ${file}`);
  }
}

console.log(`\nDone. ${totalReplaced} replacements across ${totalFilesTouched} files.`);