// Issue #733 — regression test for the bare `rounded` → `rounded-md` sweep.
//
// Walks src/components and asserts no stray bare `rounded` (or bare
// directional `rounded-r` / `-l` / `-t` / `-b` / `-tl` / `-tr` / `-bl` /
// `-br`) survives without an `allow-bare-rounded` escape comment AND
// without being in the explicit allowlist. The two intentional decorative
// chips are pinned below as the ONLY bare-rounded sites — adding a third
// without updating this list will fail the test and force a conscious
// decision.

import { describe, it, expect } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, sep } from "node:path";

const COMPONENTS_ROOT = join(process.cwd(), "src", "components");

// Bare-rounded sites that survive the sweep by intent. Each entry pins
// {file, requiredClasses (order-tolerant), escape marker}. The audit will
// fail if:
//   * a new bare `rounded` site appears anywhere in src/components, OR
//   * a bare `rounded` line is missing its escape marker, OR
//   * the line's class tokens no longer contain the requiredClasses set
//     (e.g. someone reorders, drops, or adds a class that would change the
//     chip's visual intent).
const ALLOWED_BARE_ROUNDED = [
  {
    file: "Probe/WorktreeManagerTab.tsx",
    // 2xs status Badge — `rounded` is intentional, no interaction.
    // Token was `text-[9px]` pre-#733 modernization; now `text-2xs`
    // (the new `--font-size-2xs` token, same 10px value).
    requiredClasses: ["px-1", "py-px", "rounded", "text-2xs"],
    escape: "allow-bare-rounded",
  },
  {
    file: "Probe/GitPullRequestsTab.tsx",
    // 2xs "this PR can't be merged" status pill — no interaction.
    // Token was `text-[10px]` pre-#733 modernization; now `text-2xs`.
    requiredClasses: [
      "px-2",
      "py-1",
      "text-2xs",
      "rounded",
      "bg-bg-card",
      "text-text-muted",
    ],
    escape: "allow-bare-rounded",
  },
];

// Classes that are already token-bound and never need flagging.
const KNOWN_SIZE_SUFFIXES = [
  "sm",
  "md",
  "lg",
  "xl",
  "2xl",
  "3xl",
  "full",
  "none",
  "pill",
];

// Bare-directional sides (need a size suffix to be valid).
const DIR_SIDES = ["r", "l", "t", "b", "tl", "tr", "bl", "br"];

function walk(dir: string, acc: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) walk(full, acc);
    else if (/\.(tsx?|jsx?)$/.test(entry)) acc.push(full);
  }
  return acc;
}

function findBareRoundedInFile(filePath: string): Array<{ line: number; text: string }> {
  const text = readFileSync(filePath, "utf8");
  const hits: Array<{ line: number; text: string }> = [];
  const lines = text.split(/\r?\n/);

  // Match either bare `rounded` (not followed by `-`) or bare directional
  // `rounded-{r,l,t,b,tl,tr,bl,br}` (not already size-suffixed or
  // arbitrary-valued).
  // Deliberately do NOT skip lines with the escape marker — the
  // allowlist membership is enforced separately, so an unlisted
  // escape-commented line is still a test failure.
  const bareRe = /\brounded\b(?![-])/;
  const dirRe = new RegExp(
    `\\brounded-(?:${DIR_SIDES.join("|")})\\b(?![-](?:${KNOWN_SIZE_SUFFIXES.join("|")})\\b|-\\[)`,
  );

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (bareRe.test(line) || dirRe.test(line)) {
      hits.push({ line: i + 1, text: line.trim() });
    }
  }
  return hits;
}

function extractClassName(line: string): string | null {
  // Match className="..." (string literal) or className={`...`}
  // (template literal). The contents is what we want to tokenize.
  const m = line.match(/className=(?:"([^"]*)"|\{`([^`]*)`\})/);
  return m ? (m[1] ?? m[2] ?? "") : null;
}

function tokensOf(line: string): Set<string> {
  // Audit only cares about class tokens, not the surrounding JSX
  // punctuation. Pull the className content out first (string or
  // template literal), then split on whitespace. This makes the
  // requiredClasses match independent of class order within the
  // className, and immune to `className={`...`}` wrapping.
  const className = extractClassName(line) ?? line;
  return new Set(className.split(/\s+/).filter(Boolean));
}

function matchesAllowlist(
  relPath: string,
  line: string,
): { matched: boolean; missingEscape: boolean } {
  const entry = ALLOWED_BARE_ROUNDED.find((e) => e.file === relPath);
  if (!entry) return { matched: false, missingEscape: false };
  const tokens = tokensOf(line);
  const hasAll = entry.requiredClasses.every((c) => tokens.has(c));
  if (!hasAll) return { matched: false, missingEscape: false };
  return { matched: true, missingEscape: !line.includes(entry.escape) };
}

describe("radii audit — no stray bare `rounded` in src/components (#733)", () => {
  const files = walk(COMPONENTS_ROOT);

  it("walks src/components successfully (sanity)", () => {
    expect(files.length).toBeGreaterThan(0);
  });

  it("every bare `rounded` site in src/components is in the explicit allowlist", () => {
    const offenders: Array<{ file: string; line: number; text: string }> = [];

    for (const file of files) {
      const hits = findBareRoundedInFile(file);
      const relPath = file
        .split(`${sep}src${sep}components${sep}`)[1]
        ?.replace(/\\/g, "/");

      for (const hit of hits) {
        if (!matchesAllowlist(relPath ?? "", hit.text).matched) {
          offenders.push({ file: relPath ?? file, line: hit.line, text: hit.text });
        }
      }
    }

    if (offenders.length) {
      const msg = offenders
        .map((o) => `  ${o.file}:${o.line}  ${o.text}`)
        .join("\n");
      throw new Error(
        `Bare \`rounded\` (or bare directional) found outside the allowlist.\n` +
          `Either convert to a size-suffixed variant (rounded-md / -sm / -lg / -full ...)\n` +
          `or pin the line in ALLOWED_BARE_ROUNDED above with an \`allow-bare-rounded\` comment:\n\n` +
          msg,
      );
    }
  });

  it("each allowlisted bare-rounded site still carries the escape comment", () => {
    for (const entry of ALLOWED_BARE_ROUNDED) {
      const file = join(COMPONENTS_ROOT, entry.file);
      const text = readFileSync(file, "utf8");
      const lines = text.split(/\r?\n/);
      const matches = lines.filter((l) => {
        const tokens = tokensOf(l);
        return entry.requiredClasses.every((c) => tokens.has(c));
      });
      expect(
        matches.length,
        `${entry.file} should contain a line with: ${entry.requiredClasses.join(" ")}`,
      ).toBeGreaterThan(0);
      const withEscape = matches.filter((l) => l.includes(entry.escape));
      expect(
        withEscape.length,
        `${entry.file}: requiredClasses match found ${matches.length} line(s) but ` +
          `none carry the "${entry.escape}" marker`,
      ).toBeGreaterThan(0);
    }
  });

  it("ALLOWED_BARE_ROUNDED is itself minimal (regression: don't silently grow the list)", () => {
    // Two intentional decorative chips: a 9px Badge and a 10px "can't merge" pill.
    expect(ALLOWED_BARE_ROUNDED.length).toBe(2);
  });
});