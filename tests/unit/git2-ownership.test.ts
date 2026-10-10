// Pins the git2 seam from docs/development/module-map.md.
//
// Production code outside src-tauri/src/git/ must not open libgit2 itself.
// The allowlist is the debt tracked by issue #2194. A new file that mentions
// git2:: fails this test. Removing git2 from an allowlisted file fails it
// too, until that path is deleted here and from the map's seam-debt list.
// Test-only use is allowed: a *_tests.rs file, a tests.rs module file, or
// an item marked #[cfg(test)].

import { describe, it, expect } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, sep } from "node:path";

const ROOT = process.cwd();
const SRC = join(ROOT, "src-tauri", "src");

/** Paths relative to src-tauri/src. Keep in step with the seam-debt list. */
const ALLOWED_PRODUCTION_GIT2 = [
  "circuit/verification.rs",
  "commands/ai_context.rs",
  "commands/build_run.rs",
  "commands/diff.rs",
  "commands/git.rs",
  "commands/pr.rs",
  "commands/prune.rs",
];

function walk(dir: string, acc: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) walk(full, acc);
    else if (entry.endsWith(".rs")) acc.push(full);
  }
  return acc;
}

function relativeSrc(full: string): string {
  const rel = full.slice(SRC.length + 1).split(sep).join("/");
  return rel;
}

function isTestOnlyFile(rel: string): boolean {
  return rel.endsWith("_tests.rs") || rel.endsWith("/tests.rs") || rel === "tests.rs";
}

function isCfgTestAttr(line: string): boolean {
  return /^\s*#\[cfg\((?:test\b|all\([^)\]]*?\btest\b)/.test(line);
}

/** Drop items attributed with #[cfg(test)], so fixtures are not production use. */
export function stripCfgTestItems(source: string): string {
  const lines = source.split(/\r?\n/);
  const kept: string[] = [];
  let i = 0;
  while (i < lines.length) {
    if (!isCfgTestAttr(lines[i])) {
      kept.push(lines[i]);
      i += 1;
      continue;
    }
    let j = i + 1;
    while (j < lines.length && /^\s*#\[/.test(lines[j])) j += 1;
    while (j < lines.length && lines[j].trim() === "") j += 1;
    if (j >= lines.length) break;
    let depth = 0;
    let seenBrace = false;
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
        k += 1;
        break;
      }
      if (seenBrace && depth <= 0) {
        k += 1;
        break;
      }
    }
    i = k;
  }
  return kept.join("\n");
}

function productionUsesGit2(rel: string): boolean {
  const text = stripCfgTestItems(readFileSync(join(SRC, ...rel.split("/")), "utf8"));
  return text.includes("git2::");
}

function seamDebtPaths(): string[] {
  const map = readFileSync(join(ROOT, "docs", "development", "module-map.md"), "utf8");
  const section = map.split(/^## Seam debt\s*$/m)[1]?.split(/^## /m)[0] ?? "";
  return [...section.matchAll(/`([^`\s]*\/[^`]+\.rs)`/g)].map((match) => match[1]);
}

describe("git2 ownership", () => {
  it("strips a #[cfg(test)] module but keeps production use above it", () => {
    const source = [
      "fn open() { let _ = git2::Repository::open(path); }",
      "#[cfg(test)]",
      "mod tests {",
      "    fn fixture() { let _ = git2::Repository::init(path); }",
      "}",
      "fn after() {}",
    ].join("\n");
    const stripped = stripCfgTestItems(source);
    expect(stripped).toContain("fn open()");
    expect(stripped).toContain("fn after()");
    expect(stripped).not.toContain("Repository::init");
  });

  it("does not strip #[cfg(not(test))]", () => {
    const source = "#[cfg(not(test))]\nfn open() { git2::Repository::open(path); }\n";
    expect(stripCfgTestItems(source)).toContain("git2::Repository::open");
  });

  it("matches the module map, and no other production file opens git2", () => {
    const found = walk(SRC)
      .map(relativeSrc)
      .filter((rel) => !rel.startsWith("git/") && !isTestOnlyFile(rel))
      .filter(productionUsesGit2)
      .sort();
    const allowed = [...ALLOWED_PRODUCTION_GIT2].sort();
    expect(found, `production git2 outside git/:\n${found.join("\n")}`).toEqual(allowed);
    expect(seamDebtPaths().sort()).toEqual(allowed);
  });
});
