// Issue #2450 — the dead-code / unused-import suppression policy.
//
// Walks src-tauri/src and fails on the two shapes the purge removed:
//
//   1. `allow(unused_imports)` anywhere. There is no live case left: an
//      unused module-root re-export is a name no call site wanted, and the
//      fix is to drop the name from the `pub use` slab, not to silence it.
//   2. `allow(dead_code)` without a written justification AND a tracked
//      issue in the surrounding comment window. A bare attribute is an
//      unreadable assertion that a dead item is still worth carrying; an
//      allow justified by "the tests use it" is a claim about the release
//      binary that `dead_code` never checked (it fires per configuration).
//
// The window is the attribute line plus the four lines above it and the one
// line below, because a justification is written either as a trailing
// `// ...` comment or as the doc comment on the item.

import { describe, it, expect } from "vitest";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";

const RUST_ROOT = join(process.cwd(), "src-tauri", "src");

// `allow(...)` / `expect(...)` with the lint anywhere in the list —
// `allow(dead_code)`, `allow(dead_code, clippy::too_many_arguments)` and
// `allow(clippy::foo, dead_code)` must all match. Anchoring on the lint alone
// (rather than on the whole `allow(lint)` group) is what closes that gap.
const SUPPRESSED_ATTRIBUTE = /(allow|expect)\(([^)]*)\)/;
// Outer attributes (`#[...]`) and inner ones (`#![...]` on a module or crate).
const ATTRIBUTE_LINE = /^\s*#!?\[/;
const SUPPRESSED = /\b(dead_code|unused_imports)\b/;
const ISSUE_REFERENCE = /#\d+/;
const CONTEXT_ABOVE = 4;
const CONTEXT_BELOW = 1;

function rustFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return rustFiles(path);
    return entry.name.endsWith(".rs") ? [path] : [];
  });
}

type Finding = { file: string; line: number; lint: string; window: string };

function scan(text: string): Finding[] {
  const lines = text.split(/\r?\n/);
  const findings: Finding[] = [];
  lines.forEach((raw, index) => {
    // A doc comment or prose mention is not an attribute. `/// #[allow(..)]`
    // and `// `#[allow(dead_code)]` retired` must not trip the guard.
    if (!ATTRIBUTE_LINE.test(raw)) return;
    // Only the lint list inside `allow(…)` counts, so an unrelated attribute
    // that merely mentions a lint name does not trip it.
    const parts = raw.match(SUPPRESSED_ATTRIBUTE);
    if (!parts) return;
    const [, keyword, body] = parts;
    const lints = body
      .split(",")
      .map((part) => part.trim())
      .filter((lint) => SUPPRESSED.test(lint));
    if (!lints?.length) return;
    const lint = lints[0];
    const window = lines
      .slice(Math.max(0, index - CONTEXT_ABOVE), index + 1 + CONTEXT_BELOW)
      .join("\n");
    // `#[expect(lint, reason = "…")]` is the compiler-enforced spelling: the
    // build fails once the lint stops firing, and the `reason` is the written
    // justification. It is accepted on that basis; an `expect` without a
    // reason is as bare as an `allow` and is still flagged.
    const justifiedByReason = keyword === "expect" && /\breason\s*=\s*"[^"]+"/.test(body);
    if (lint === "unused_imports" || (!justifiedByReason && !ISSUE_REFERENCE.test(window))) {
      findings.push({ file: "", line: index + 1, lint, window });
    }
  });
  return findings;
}

const files = rustFiles(RUST_ROOT);

describe("rust suppression policy (#2450)", () => {
  it("finds Rust sources to scan", () => {
    // A walk that silently matches nothing is a green placebo.
    expect(files.length).toBeGreaterThan(50);
  });

  it("has no allow(unused_imports) and no unexplained allow(dead_code)", () => {
    const violations: string[] = [];
    for (const file of files) {
      for (const finding of scan(readFileSync(file, "utf8"))) {
        violations.push(
          `${relative(process.cwd(), file)}:${finding.line} — ` +
            `allow(${finding.lint}) without a tracked issue in the justification window.\n` +
            `    ${finding.window.split("\n").join("\n    ")}`
        );
      }
    }
    expect(violations).toEqual([]);
  });

  describe("detector", () => {
    it("flags a bare allow(dead_code)", () => {
      expect(scan("#[allow(dead_code)]\nfn orphan() {}").length).toBe(1);
    });

    it("flags allow(unused_imports) even with a justification", () => {
      const source = ["// issue #970 kept this slab", "#[allow(unused_imports)]", "pub use x::Y;"].join("\n");
      expect(scan(source).length).toBe(1);
    });

    it("accepts an allow(dead_code) justified with an issue", () => {
      const source = ["/// Retained for #970: the live path calls the _against twin.", "#[allow(dead_code)]", "fn seam() {}"].join("\n");
      expect(scan(source)).toEqual([]);
    });

    it("accepts a trailing justification comment on the attribute line", () => {
      const source = "#[allow(dead_code)] // RAII lock holder; see issue #2450.";
      expect(scan(source)).toEqual([]);
    });

    it("accepts a cfg_attr-scoped allow(dead_code) with an issue nearby", () => {
      const source = ["// ts-rs payload emitted only by the dev bridge (#2153).", "#[cfg_attr(not(any(debug_assertions, feature = \"dev-tools\")), allow(dead_code))]", "pub struct P;"].join("\n");
      expect(scan(source)).toEqual([]);
    });

    it("ignores prose that names the attribute", () => {
      const source = ["/// old `#[allow(dead_code)]` retired in #2450", "// a bare #[allow(dead_code)] is banned", "fn live() {}"].join("\n");
      expect(scan(source)).toEqual([]);
    });

    it("ignores other lints in a mixed allow", () => {
      expect(scan("#[allow(clippy::too_many_arguments)]\nfn wide() {}")).toEqual([]);
    });

    // The three gaps a reviewer found: a mixed allow naming the lint after
    // another lint, the same with the lint first, and an inner attribute.
    it("flags dead_code inside a mixed allow with another lint", () => {
      expect(scan("#[allow(dead_code, clippy::too_many_arguments)]\nfn orphan() {}").length).toBe(1);
      expect(scan("#[allow(clippy::too_many_arguments, dead_code)]\nfn orphan() {}").length).toBe(1);
    });

    it("accepts a mixed allow naming dead_code when an issue is cited", () => {
      const source = ["// #2450: only the test suite reads this.", "#[allow(clippy::too_many_arguments, dead_code)]", "fn seam() {}"].join("\n");
      expect(scan(source)).toEqual([]);
    });

    it("flags an inner attribute", () => {
      expect(scan("#![allow(dead_code)]\nfn orphan() {}").length).toBe(1);
      expect(scan("#![allow(unused_imports)]\nmod m {}").length).toBe(1);
    });

    it("flags an expect attribute without a stated reason", () => {
      // `#[expect(dead_code)]` with no `reason` is as bare as an `allow`; the
      // scan must see it rather than pass it through.
      const source = ["#[expect(dead_code)]", "fn orphan() {}"].join("\n");
      expect(scan(source).length).toBe(1);
    });

    it("accepts an expect attribute that states its reason", () => {
      // The compiler-enforced spelling: the build fails once the lint stops
      // firing, so the reason is the justification.
      const source = ["#[expect(dead_code, reason = \"Contract-test surface\")]", "fn seam() {}"].join("\n");
      expect(scan(source)).toEqual([]);
    });

    it("flags an unjustified allow that sits too far from its issue", () => {
      const source = ["// unrelated prose", "// more unrelated prose", "// still unrelated", "// nothing to do with linting", "#[allow(dead_code)]", "fn orphan() {}"].join("\n");
      expect(scan(source).length).toBe(1);
    });
  });
});