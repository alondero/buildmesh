// Shared by the Vitest parser cases and the tree walk in git2-ownership.test.mjs.
// The walk stays out of tests/unit: frontend-tests reuses a Rust-only edit, and
// a scan of every Rust file would make that reuse unsound.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, sep } from "node:path";

/** Paths relative to src-tauri/src. Keep in step with the seam-debt list. */
export const ALLOWED_PRODUCTION_GIT2 = [
  "circuit/verification.rs",
  "commands/ai_context.rs",
  "commands/build_run.rs",
  "commands/diff.rs",
  "commands/git.rs",
  "commands/pr.rs",
  "commands/prune.rs",
];

function isCfgTestAttr(line) {
  // `test` as its own predicate, including inside all(...) and any(...).
  // `not(test)` is production. A `)` inside a feature literal must not hide
  // a later `test` predicate.
  return /^\s*#\[cfg\((?:test\b|(?:all|any)\([^\]]*?(?<!not\()test\b)/.test(line);
}

/** Drop items attributed with #[cfg(test)], so fixtures are not production use. */
export function stripCfgTestItems(source) {
  const lines = source.split(/\r?\n/);
  const kept = [];
  let i = 0;
  while (i < lines.length) {
    if (!isCfgTestAttr(lines[i])) {
      kept.push(lines[i]);
      i += 1;
      continue;
    }
    // Only a whole-line attribute is skipped. An inline `#[cfg(test)] fn`
    // is the item; skipping that line would delete the next production line.
    let j = i;
    while (j < lines.length && /^\s*#\[[^\]]*\]\s*$/.test(lines[j])) j += 1;
    while (j < lines.length && lines[j].trim() === "") j += 1;
    if (j >= lines.length) {
      kept.push(...lines.slice(i));
      break;
    }
    let depth = 0;
    let seenBrace = false;
    let closed = false;
    let k = j;
    for (; k < lines.length; k += 1) {
      const line = lines[k];
      for (const ch of line) {
        if (ch === "{") {
          depth += 1;
          seenBrace = true;
        } else if (ch === "}") {
          depth -= 1;
        }
      }
      if (!seenBrace && line.includes(";")) {
        closed = true;
        k += 1;
        break;
      }
      if (seenBrace && depth <= 0) {
        closed = true;
        k += 1;
        break;
      }
    }
    // Braces that never return to zero used to drop every later line. Keep
    // the tail so production git2 use after a broken count stays visible.
    if (!closed) kept.push(...lines.slice(i, k));
    i = k;
  }
  return kept.join("\n");
}

function walk(dir, acc = []) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) walk(full, acc);
    else if (entry.endsWith(".rs")) acc.push(full);
  }
  return acc;
}

function isTestOnlyFile(rel) {
  return rel.endsWith("_tests.rs") || rel.endsWith("/tests.rs") || rel === "tests.rs";
}

export function productionGit2Paths(root) {
  const src = join(root, "src-tauri", "src");
  return walk(src)
    .map((full) => full.slice(src.length + 1).split(sep).join("/"))
    .filter((rel) => !rel.startsWith("git/") && !isTestOnlyFile(rel))
    .filter((rel) => stripCfgTestItems(readFileSync(join(src, ...rel.split("/")), "utf8")).includes("git2::"))
    .sort();
}

export function seamDebtPaths(root) {
  const map = readFileSync(join(root, "docs", "development", "module-map.md"), "utf8");
  const section = map.split(/^## Seam debt\s*$/m)[1]?.split(/^## /m)[0] ?? "";
  return [...section.matchAll(/`([^`\s]*\/[^`]+\.rs)`/g)].map((match) => match[1]);
}
