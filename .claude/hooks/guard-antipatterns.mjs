#!/usr/bin/env node
// PreToolUse guard: blocks edits that introduce catastrophic anti-patterns.
// See CLAUDE.md "Hard rules". Per-line escape hatches: `// allow-dispose`, `// allow-wsl-path`.
// Reads the Claude Code hook payload from stdin. Exit 2 + stderr = block the tool call.
// Fails open (exit 0) on any parse error so it can never wedge all edits.
//
// Pure helpers are exported for unit tests (tests/unit/guard-antipatterns.test.ts);
// main() only runs when this file is executed directly as a hook.

import { readFileSync } from "node:fs";

function readStdin() {
  try {
    return readFileSync(0, "utf8");
  } catch {
    return "";
  }
}

export function collectNewText(input) {
  // Returns the text the edit is trying to introduce, across Edit/Write/MultiEdit.
  if (typeof input?.content === "string") return input.content;
  if (typeof input?.new_string === "string") return input.new_string;
  if (Array.isArray(input?.edits)) {
    return input.edits.map((e) => e?.new_string ?? "").join("\n");
  }
  return "";
}

const RULES = [
  {
    id: "terminal-dispose",
    appliesTo: (path) => /Terminal/.test(path) && /\.tsx?$/.test(path),
    pattern: /\.dispose\s*\(/,
    allow: "allow-dispose",
    message:
      "Calling .dispose() on an xterm.js terminal causes permanent terminal blanking. " +
      "TerminalManager is a singleton whose instances must survive React remounts. " +
      "Only dispose when the agent node is actually deleted — if so, add `// allow-dispose` on that line.",
  },
  {
    id: "wsl-path-outside-env",
    appliesTo: (path) => /\.rs$/.test(path) && !/(^|\/)src-tauri\/src\/env\//.test(path),
    pattern: /\\+wsl\$/,
    allow: "allow-wsl-path",
    message:
      "WSL UNC paths (\\\\wsl$\\...) must only be constructed inside src-tauri/src/env/. " +
      "Route path conversion through env::to_host_path instead of hand-building paths here — " +
      "if this really is the right place, add `// allow-wsl-path` on that line.",
  },
];

export function checkContentViolations(filePath, newText) {
  // Returns an array of violation messages for the content-pattern rules.
  const normPath = filePath.replace(/\\/g, "/");
  const lines = newText.split("\n");
  const violations = [];
  for (const rule of RULES) {
    if (!rule.appliesTo(normPath)) continue;
    for (const line of lines) {
      if (rule.pattern.test(line) && !line.includes(rule.allow)) {
        violations.push(`[${rule.id}] ${rule.message}`);
        break; // one report per rule is enough
      }
    }
  }
  return violations;
}

export function checkWorktreeEscape(filePath, cwd) {
  // Catches the #1 recurring Windows trap: a session running inside a git worktree
  // (…/.claude/worktrees/<name>/) editing an ABSOLUTE path that lands in the main
  // checkout instead. The edit silently goes to the wrong branch and `cargo test`
  // greens against the unchanged tree. Returns a message string, or null if safe.
  // Set BUILDMESH_ALLOW_WORKTREE_ESCAPE=1 to deliberately edit outside the worktree.
  if (process.env.BUILDMESH_ALLOW_WORKTREE_ESCAPE === "1") return null;
  if (!filePath || !cwd) return null;

  const fp = filePath.replace(/\\/g, "/");
  // Edit/Write require absolute paths; relative paths resolve against cwd (the
  // worktree) and are inherently safe, so only police absolute paths.
  const isAbsolute = /^([a-zA-Z]:\/|\/)/.test(fp);
  if (!isAbsolute) return null;

  const normCwd = cwd.replace(/\\/g, "/");
  const wt = normCwd.match(/^(.*?)([/\\]?\.claude\/worktrees\/[^/]+)/i);
  if (!wt) return null; // not inside a worktree — nothing to check

  const mainRoot = wt[1]; // e.g. X:/src/buildmesh
  const worktreeRoot = wt[1] + wt[2]; // e.g. X:/src/buildmesh/.claude/worktrees/red-rare-hedge

  const fpLc = fp.toLowerCase();
  const mainPrefix = (mainRoot + "/").toLowerCase();
  const wtPrefix = (worktreeRoot + "/").toLowerCase();

  if (fpLc.startsWith(mainPrefix) && !fpLc.startsWith(wtPrefix)) {
    return (
      "[worktree-escape] This session is inside the worktree:\n  " +
      worktreeRoot +
      "\nbut the edit targets a path in the main checkout (or another worktree):\n  " +
      fp +
      "\nOn Windows the file tools write to this path VERBATIM, so the edit lands on the\n" +
      "wrong branch and your worktree tests will green against an unchanged tree.\n" +
      "Rewrite the path under the worktree root above. If you truly mean to edit outside\n" +
      "the worktree, re-run with BUILDMESH_ALLOW_WORKTREE_ESCAPE=1 in the environment."
    );
  }
  return null;
}

function main() {
  const raw = readStdin();
  if (!raw.trim()) process.exit(0);

  let payload;
  try {
    payload = JSON.parse(raw);
  } catch {
    process.exit(0);
  }

  const filePath = payload?.tool_input?.file_path ?? "";
  const newText = collectNewText(payload?.tool_input);
  if (!filePath) process.exit(0);

  const violations = [];

  const escape = checkWorktreeEscape(filePath, payload?.cwd ?? "");
  if (escape) violations.push(escape);

  if (newText) violations.push(...checkContentViolations(filePath, newText));

  if (violations.length) {
    process.stderr.write(
      "Blocked by buildmesh anti-pattern guard (.claude/hooks/guard-antipatterns.mjs):\n\n" +
        violations.join("\n\n") +
        "\n",
    );
    process.exit(2);
  }

  process.exit(0);
}

// Run as a hook in production; skip when imported by the Vitest test runner
// (Vitest sets process.env.VITEST), so importing the pure helpers never calls
// process.exit. Avoids fragile argv-vs-import.meta.url path comparison on Windows.
if (!process.env.VITEST) main();
