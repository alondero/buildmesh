#!/usr/bin/env node
// PostToolUse guard: after an Edit/Write/MultiEdit "succeeds", confirm the file was
// actually written to disk just now. On Windows worktree sessions the file tools
// sometimes report success while the disk file is unchanged (cache/persistence trap)
// or the write landed at a different path — and Read/Grep read the same phantom
// cache, so only the filesystem tells the truth. Exit 2 + stderr surfaces a warning
// back to the model (the tool already ran) telling it to verify with `git diff --stat`.
// Fails open (exit 0) on any uncertainty so it can never wedge editing.
//
// We check the file's MODIFICATION TIME, not its contents. An earlier content-substring
// version false-fired constantly: any benign byte transform the tools apply (em-dash
// 0x97->U+2014, smart quotes, EOL) makes a whole-content match fail even though the edit
// landed fine. "Was this file written in the last few seconds?" is encoding-immune and
// answers exactly the question that matters — did the write reach disk at this path.
//
// Pure helpers are exported for unit tests (tests/unit/verify-edit-persisted.test.ts);
// main() only runs when this file is executed directly as a hook.

import { readFileSync, statSync } from "node:fs";

function readStdin() {
  try {
    return readFileSync(0, "utf8");
  } catch {
    return "";
  }
}

// Generous window: PostToolUse fires within milliseconds of the write, so anything
// older than this means the edit did not touch this path. Wide enough to absorb slow
// disks / clock granularity without crying wolf.
export const FRESH_WINDOW_MS = 15000;

export function checkRecentlyWritten(filePath, statFile, nowMs, windowMs = FRESH_WINDOW_MS) {
  // Returns a warning if the file's mtime is older than the window, else null.
  // `statFile`/`nowMs` are injectable for tests.
  if (!filePath) return null;

  let st;
  try {
    st = statFile(filePath);
  } catch (err) {
    // PostToolUse only fires after a SUCCESSFUL edit, so the file must exist now.
    // ENOENT means the write never landed at this path (e.g. a new-file Write that
    // silently went nowhere) — exactly the trap, so warn. Any other stat error
    // (permissions, etc.) is unrelated; fail open.
    if (err && err.code === "ENOENT") {
      return (
        "[edit-not-persisted] The edit reported success but no file exists on disk at:\n  " +
        filePath +
        "\nThe write did not land at this path. Confirm you are editing the worktree path " +
        "(not the main checkout) and re-apply; verify with `git --no-pager diff --stat`."
      );
    }
    return null; // unreadable for unrelated reasons — fail open
  }

  const ageMs = nowMs - st.mtimeMs;
  if (ageMs > windowMs) {
    const ageSec = Math.round(ageMs / 1000);
    return (
      "[edit-not-persisted] The edit reported success but the file on disk was NOT\n" +
      "modified just now (last written ~" +
      ageSec +
      "s ago) at:\n  " +
      filePath +
      "\nThis is the Windows worktree persistence/path trap: the tool's success does not\n" +
      "mean the bytes hit disk on the branch you think. Verify with `git --no-pager diff\n" +
      "--stat` from the worktree, confirm you are editing the worktree path (not the main\n" +
      "checkout), and re-apply if needed. Do NOT trust a green test run until the diff\n" +
      "shows your change."
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
  const warning = checkRecentlyWritten(filePath, (p) => statSync(p), Date.now());

  if (warning) {
    process.stderr.write(
      "Buildmesh edit-persistence check (.claude/hooks/verify-edit-persisted.mjs):\n\n" +
        warning +
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
