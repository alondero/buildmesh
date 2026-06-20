#!/usr/bin/env node
// PreToolUse(Bash) guard: stop an empty/aspirational `git commit`.
// The #491->#504 incident shipped a commit with nothing meaningfully staged; the PR
// body described 19 files, the commit held ~0. This denies a *plain* commit (one that
// does not stage inline) when the staged set is empty, and shows the working tree so
// the model stages the right files and re-commits.
//
// Scope & why it's narrow: PreToolUse fires BEFORE the command runs, so for
// `git add ... && git commit` the staged state we'd read is the PRE-add state — we
// can't judge it. So we only evaluate a *plain* commit (no `git add`, no `-a/--all`,
// no `--amend`, no `--allow-empty`), where the current staged set is exactly what will
// be committed. "Nothing staged" is the one case that is never a legitimate commit and
// never stalls an AFK agent (deny is immediate; the model just stages and retries).
// It deliberately does NOT try to judge under-staging (1-of-N) — that needs intent the
// hook can't see; the `git diff --staged --stat` habit in CLAUDE.md covers that.
// Fails open (allow) on any parse/git error so it can never wedge committing.
//
// Pure helpers are exported for unit tests (tests/unit/guard-commit-staging.test.ts);
// main() only runs when executed directly as a hook.

import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";

function readStdin() {
  try {
    return readFileSync(0, "utf8");
  } catch {
    return "";
  }
}

// Shell separators that break a command line into independently-running segments.
const SEGMENT_SEP = /&&|\|\||[;&|\n]/;
// `git [global-opts] commit` as a REAL subcommand. Only dash-options and the two
// arg-taking global opts (`-C <path>`, `-c <name=val>`) may sit between `git` and
// `commit` — so `git push origin commit-branch` (a non-dash token before a word
// containing "commit") is NOT matched and can never be mistaken for a commit.
const GIT_COMMIT_RE = /(^|\s)git\s+(?:-[Cc]\s+\S+\s+|-\S+\s+)*commit\b/;
const GIT_ADD_RE = /(^|\s)git\s+(?:-[Cc]\s+\S+\s+|-\S+\s+)*(?:add|stage)\b/;

export function classifyCommand(cmd) {
  const c = String(cmd ?? "");
  // Scope detection to the segment that actually runs `git commit`. Scanning the whole
  // command string mis-fires: another piped command's flags (`ls -la && git commit`) or
  // a quoted message ("run git add first") would otherwise be read as commit flags.
  const segments = c.split(SEGMENT_SEP);
  const commitIdx = segments.findIndex((s) => GIT_COMMIT_RE.test(s));
  if (commitIdx === -1) return { isCommit: false, isPlainCommit: false };

  const seg = segments[commitIdx];
  // These are flags of the commit itself → scan only the commit segment.
  // No leading \b before `--flag`: `-` is a non-word char, so a space-then-dash has no
  // word boundary and `\b--amend` would never match.
  const isAmend = /--amend\b/.test(seg);
  const isAllowEmpty = /--allow-empty\b/.test(seg);
  // commit -a / --all / -am etc. auto-stage tracked files, so the staged snapshot won't
  // reflect what gets committed — treat as inline staging and skip.
  const hasAllFlag = /\s--all\b/.test(seg) || /\s-[A-Za-z]*a[A-Za-z]*\b/.test(seg);
  // A `git add`/`git stage` EARLIER in the same chain stages before the commit runs, so
  // the pre-command staged snapshot can't be judged — skip. (Check earlier segments, not
  // the message text in the commit segment.)
  const hasAddBefore = segments.slice(0, commitIdx).some((s) => GIT_ADD_RE.test(s));

  const isPlainCommit = !hasAddBefore && !isAmend && !isAllowEmpty && !hasAllFlag;
  return { isCommit: true, isPlainCommit };
}

export function decide(cmd, gitStateProvider) {
  // Returns a PreToolUse hookSpecificOutput body (deny) or null (allow).
  const { isCommit, isPlainCommit } = classifyCommand(cmd);
  if (!isCommit || !isPlainCommit) return null;

  let state;
  try {
    state = gitStateProvider();
  } catch {
    return null; // not a repo / git failed — fail open
  }
  if (!state || state.stagedCount > 0) return null;

  return {
    hookEventName: "PreToolUse",
    permissionDecision: "deny",
    permissionDecisionReason:
      "Nothing is staged, so this `git commit` would be empty/aspirational — the exact " +
      "shape of the #491->#504 staging-miss incident (PR body described many files, the " +
      "commit held none). Stage the intended files (`git add -A`, or specific paths) and " +
      "re-commit. Working tree right now:\n" +
      (state.statusShort ? state.statusShort : "(clean — there is nothing to commit)") +
      "\nIf an empty commit is genuinely intended, pass --allow-empty.",
  };
}

function readGitState(cwd) {
  const opts = { cwd, encoding: "utf8" };
  const staged = execFileSync("git", ["diff", "--staged", "--name-only"], opts);
  const stagedCount = staged.split("\n").filter((l) => l.trim().length > 0).length;
  const statusShort = execFileSync("git", ["status", "--short"], opts).trimEnd();
  return { stagedCount, statusShort };
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

  if (payload?.tool_name !== "Bash") process.exit(0);
  const cmd = payload?.tool_input?.command ?? "";
  const cwd = payload?.cwd || process.cwd();

  const verdict = decide(cmd, () => readGitState(cwd));
  if (verdict) {
    process.stdout.write(JSON.stringify({ hookSpecificOutput: verdict }));
  }
  process.exit(0);
}

// Run as a hook in production; skip when imported by the Vitest test runner.
if (!process.env.VITEST) main();
