// Issue #733 — regression test for the bare `rounded` → `rounded-md` sweep.
//
// Walks src/components and asserts no stray bare `rounded` (or bare
// directional `rounded-r` / `-l` / `-t` / `-b` / `-tl` / `-tr` / `-bl` /
// `-br`) survives without an `allow-bare-rounded` escape comment. The two
// intentional decorative chips are pinned below as the ONLY bare-rounded
// sites — adding a third without updating this list will fail the test and
// force a conscious decision.

import { describe, it, expect } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, sep } from "node:path";

const COMPONENTS_ROOT = join(process.cwd(), "src", "components");

// Bare-rounded sites that survive the sweep by intent. Each entry pins
// {file, line-1-indexed, snippet} so the test is order-agnostic but fails
// if any of these silently changes shape (e.g. someone deletes the escape
// comment, or shifts the radius to rounded-md).
const ALLOWED_BARE_ROUNDED = [
  {
    file: "Probe/WorktreeManagerTab.tsx",
    // Line 99: Badge component className (9px status badge, no interaction).
    lineContains: "px-1 py-px rounded text-[9px]",
    escape: "allow-bare-rounded",
  },
  {
    file: "Probe/GitPullRequestsTab.tsx",
    // Line 692: "this PR can't be merged" 10px status pill, no interaction.
    lineContains: "px-2 py-1 text-[10px] rounded bg-bg-card text-text-muted",
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
  // `rounded-{r,l,t,b,tl,tr,bl,br}` (not already size-suffixed).
  const bareRe = /\brounded\b(?![-])/;
  const dirRe = new RegExp(
    `\\brounded-(?:${DIR_SIDES.join("|")})\\b(?![-](?:${KNOWN_SIZE_SUFFIXES.join("|")})\\b)`,
  );

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (bareRe.test(line) || dirRe.test(line)) {
      // Skip lines that carry the escape comment (anywhere on the line).
      if (line.includes("allow-bare-rounded")) continue;
      hits.push({ line: i + 1, text: line.trim() });
    }
  }
  return hits;
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
        // Check whether this hit matches one of the allowed entries.
        const allowed = ALLOWED_BARE_ROUNDED.some(
          (entry) =>
            entry.file === relPath && hit.text.includes(entry.lineContains),
        );
        if (!allowed) {
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
    // Catches a follow-up edit that strips the escape comment without
    // reclassifying the radius — the guard would then start firing.
    for (const entry of ALLOWED_BARE_ROUNDED) {
      const file = join(COMPONENTS_ROOT, entry.file);
      const text = readFileSync(file, "utf8");
      const lines = text.split(/\r?\n/);
      const line = lines.find((l) => l.includes(entry.lineContains));
      expect(line, `${entry.file} should contain: ${entry.lineContains}`).toBeDefined();
      expect(line, `${entry.file}: missing escape`).toContain(entry.escape);
    }
  });

  it("ALLOWED_BARE_ROUNDED is itself minimal (regression: don't silently grow the list)", () => {
    // Two intentional decorative chips: a 9px Badge and a 10px "can't merge" pill.
    expect(ALLOWED_BARE_ROUNDED.length).toBe(2);
  });
});