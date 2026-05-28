#!/usr/bin/env node
// PreToolUse guard: blocks edits that introduce catastrophic anti-patterns.
// See CLAUDE.md "Hard rules". Per-line escape hatches: `// allow-dispose`, `// allow-wsl-path`.
// Reads the Claude Code hook payload from stdin. Exit 2 + stderr = block the tool call.
// Fails open (exit 0) on any parse error so it can never wedge all edits.

import { readFileSync } from "node:fs";

function readStdin() {
  try {
    return readFileSync(0, "utf8");
  } catch {
    return "";
  }
}

function collectNewText(input) {
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
  if (!filePath || !newText) process.exit(0);

  const normPath = filePath.replace(/\\/g, "/");
  const lines = newText.split("\n");
  const violations = [];

  for (const rule of RULES) {
    if (!rule.appliesTo(normPath)) continue;
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (rule.pattern.test(line) && !line.includes(rule.allow)) {
        violations.push(`[${rule.id}] ${rule.message}`);
        break; // one report per rule is enough
      }
    }
  }

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

main();
