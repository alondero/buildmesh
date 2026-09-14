#!/usr/bin/env node
// PreToolUse(Bash) guard: require a documentation decision before committing
// behavior-sensitive changes. The decision is pure; the hook supplies a
// read-only Git snapshot for the commit shape it detected. Ambiguous shell
// commands fail open because CI is the authoritative post-commit check.
// Use `docs: none — <reason>` in the commit message when the change genuinely
// has no documentation impact.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

const MAX_BUFFER = 32 * 1024 * 1024;
const EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

export function stripHeredocs(command) {
  return String(command ?? "").replace(
    /<<-?\s*(["']?)([A-Za-z_]\w*)\1[\s\S]*?(?:\n|^)[ \t]*\2(?=\s|$)/g,
    " <<heredoc ",
  );
}

function splitShellSegments(command) {
  const segments = [];
  let current = "";
  let quote = null;
  let escaped = false;
  const text = String(command ?? "");

  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (escaped) {
      current += character;
      escaped = false;
      continue;
    }
    if (character === "\\" && quote !== "'") {
      current += character;
      escaped = true;
      continue;
    }
    if (quote) {
      current += character;
      if (character === quote) quote = null;
      continue;
    }
    if (character === "'" || character === '"') {
      quote = character;
      current += character;
      continue;
    }
    if (character === "\n" || character === ";" || character === "&" || character === "|") {
      if ((character === "&" || character === "|") && text[index + 1] === character) index += 1;
      segments.push(current);
      current = "";
      continue;
    }
    current += character;
  }
  segments.push(current);
  return segments.filter((segment) => segment.trim());
}

function shellWords(segment) {
  const words = [];
  let current = "";
  let quote = null;
  let escaped = false;
  let inWord = false;
  const text = String(segment ?? "");

  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (escaped) {
      current += character;
      escaped = false;
      inWord = true;
      continue;
    }
    const nextCharacter = text[index + 1];
    const canEscape = /[\s\\"';&|<>()[\]{}]/.test(nextCharacter ?? "");
    if (character === "\\" && quote !== "'" && canEscape) {
      escaped = true;
      inWord = true;
      continue;
    }
    if (quote) {
      if (character === quote) quote = null;
      else current += character;
      inWord = true;
      continue;
    }
    if (character === "'" || character === '"') {
      quote = character;
      inWord = true;
      continue;
    }
    if (/\s/.test(character)) {
      if (inWord) {
        words.push(current);
        current = "";
        inWord = false;
      }
      continue;
    }
    current += character;
    inWord = true;
  }
  if (escaped) current += "\\";
  if (inWord) words.push(current);
  return words;
}

const COMMIT_ARG_OPTIONS = new Set([
  "-C",
  "-F",
  "-c",
  "--author",
  "--cleanup",
  "--date",
  "--encoding",
  "--file",
  "--gpg-sign",
  "--message",
  "--reedit-message",
  "--reuse-message",
  "--trailer",
]);

function commitFlagArguments(args) {
  const flags = [];
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--") break;
    if (COMMIT_ARG_OPTIONS.has(argument)) {
      index += 1;
      continue;
    }
    if (argument.startsWith("--") && argument.includes("=")) continue;
    if (argument.startsWith("-m") && argument.length > 2) continue;
    flags.push(argument);
  }
  return flags;
}

function findGitSubcommand(segment, expected) {
  const words = shellWords(segment);
  if (!/^(?:git|git\.exe)$/i.test(words[0] ?? "")) return null;

  let index = 1;
  while (index < words.length) {
    const word = words[index];
    if (word === expected) return { words, argumentIndex: index + 1 };
    if (word === "-C" || word === "-c" || word === "--config-env") {
      index += 2;
      continue;
    }
    if (word.startsWith("-") && word !== "-") {
      index += 1;
      continue;
    }
    return null;
  }
  return null;
}

export function classifyCommitCommand(command) {
  const segments = splitShellSegments(stripHeredocs(command));
  const commitIndex = segments.findIndex((segment) => findGitSubcommand(segment, "commit"));
  if (commitIndex === -1) return { isCommit: false, isPlainCommit: false };

  const commit = findGitSubcommand(segments[commitIndex], "commit");
  const commitArgs = commit.words.slice(commit.argumentIndex);
  const addCommands = segments
    .slice(0, commitIndex)
    .map((segment) => findGitSubcommand(segment, "add") ?? findGitSubcommand(segment, "stage"))
    .filter(Boolean);
  const commitFlags = commitFlagArguments(commitArgs);
  const hasAutoStage = commitFlags.some((argument) =>
    argument === "--all" || (argument.startsWith("-") && !argument.startsWith("--") && argument.includes("a")),
  );
  const hasAmend = commitFlags.some((argument) => argument === "--amend" || argument.startsWith("--amend="));
  const hasAllowEmpty = commitFlags.some((argument) => argument === "--allow-empty");
  const hasAddBefore = addCommands.length > 0;
  const pathspecSeparator = commitArgs.indexOf("--");

  return {
    isCommit: true,
    isPlainCommit: !hasAddBefore && !hasAutoStage && !hasAmend && !hasAllowEmpty,
    hasAddBefore,
    hasAutoStage,
    hasAmend,
    hasAllowEmpty,
    commitSegment: segments[commitIndex],
    commitWords: commit.words,
    commitArgumentIndex: commit.argumentIndex,
    commitPathspecs: pathspecSeparator === -1 ? [] : commitArgs.slice(pathspecSeparator + 1),
    addCommands,
  };
}

export function isDocumentationPath(path) {
  const normalised = String(path).replaceAll("\\", "/");
  return /^(?:docs\/.*\.md$|\.github\/.*\.md$|README\.md$|CONTRIBUTING\.md$|CONTEXT\.md$|SECURITY\.md$|CODE_OF_CONDUCT\.md$|CHANGELOG\.md$)/i.test(normalised);
}

export function isBehaviorSensitivePath(path) {
  const normalised = String(path).replaceAll("\\", "/");
  if (isDocumentationPath(normalised)) return false;
  if (/^(?:tests\/|\.codex\/|src\/types\/generated\/)/i.test(normalised)) return false;
  if (/^(?:scripts\/check(?:\.ps1|-.*\.mjs)$|\.claude\/|\.github\/workflows\/|package(?:-lock)?\.json$|src-tauri\/(?:Cargo\.toml|tauri[^/]*\.json)$|(?:eslint|vite|tsconfig)[^/]*\.)/i.test(normalised)) return true;
  return /^(?:src\/|src-tauri\/src\/)/i.test(normalised);
}

function hasReasonedExemption(text) {
  const match = String(text ?? "").match(/docs:\s*none\b([\s\S]*)/i);
  return Boolean(match && /[A-Za-z0-9]/.test(match[1] ?? ""));
}

function commitMessageValues(classification) {
  const args = classification.commitWords.slice(classification.commitArgumentIndex);
  const values = [];
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "-m" || argument === "--message") {
      if (args[index + 1] !== undefined) values.push({ kind: "message", value: args[++index] });
    } else if (argument.startsWith("--message=")) {
      values.push({ kind: "message", value: argument.slice("--message=".length) });
    } else if (argument.startsWith("-m") && argument.length > 2) {
      values.push({ kind: "message", value: argument.slice(2) });
    } else if (argument === "-am") {
      if (args[index + 1] !== undefined) values.push({ kind: "message", value: args[++index] });
    } else if (argument.startsWith("-am")) {
      values.push({ kind: "message", value: argument.slice(3) });
    } else if (argument === "-F" || argument === "--file") {
      if (args[index + 1] !== undefined) values.push({ kind: "file", value: args[++index] });
    } else if (argument.startsWith("--file=")) {
      values.push({ kind: "file", value: argument.slice("--file=".length) });
    }
  }
  return values;
}

function heredocBodies(command) {
  return [...String(command ?? "").matchAll(/<<-?\s*(["']?)([A-Za-z_]\w*)\1\r?\n([\s\S]*?)\r?\n[ \t]*\2(?=\s|$)/g)].map((match) => match[3]);
}

export function hasDocumentationExemption(command, { cwd = process.cwd() } = {}) {
  const text = String(command ?? "");
  const classification = classifyCommitCommand(text);
  if (!classification.isCommit) return hasReasonedExemption(text);

  for (const value of commitMessageValues(classification)) {
    if (value.kind === "message" && hasReasonedExemption(value.value)) return true;
    if (value.kind === "file" && value.value !== "-") {
      try {
        if (hasReasonedExemption(readFileSync(resolve(cwd, value.value), "utf8"))) return true;
      } catch {
        // The commit command will report an unreadable message file itself.
      }
    }
    if (value.kind === "file" && value.value === "-") {
      if (heredocBodies(text).some(hasReasonedExemption)) return true;
    }
  }
  return false;
}

function addSelection(addCommand) {
  const args = addCommand.words.slice(addCommand.argumentIndex);
  const pathspecs = [];
  let mode = "paths";
  let afterDoubleDash = false;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (afterDoubleDash) {
      pathspecs.push(argument);
      continue;
    }
    if (argument === "--") {
      afterDoubleDash = true;
    } else if (argument === "-A" || argument === "--all") {
      mode = "all";
    } else if (argument === "-u" || argument === "--update") {
      mode = "tracked";
    } else if (argument === "-p" || argument === "--patch") {
      // The selected hunk is interactive and cannot be known before the
      // command runs, even when a path follows it.
      return null;
    } else if (argument === "-i" || argument === "--interactive" || argument === "--pathspec-from-file") {
      return null;
    } else if (argument.startsWith("--pathspec-from-file=")) {
      return null;
    } else if (!argument.startsWith("-")) {
      pathspecs.push(argument);
    }
  }
  if (pathspecs.length > 0) return { mode: "paths", pathspecs };
  if (mode === "all" || mode === "tracked") return { mode };
  return null;
}

function readNulNames(cwd, args) {
  return execFileSync("git", args, { cwd, encoding: "utf8", maxBuffer: MAX_BUFFER })
    .split("\0")
    .filter(Boolean);
}

function readStatusNames(cwd) {
  const entries = readNulNames(cwd, ["status", "--porcelain=v1", "-z"]);
  const names = [];
  for (let index = 0; index < entries.length; index += 1) {
    const entry = entries[index];
    const status = entry.slice(0, 2);
    const name = entry.slice(3);
    if (name) names.push(name);
    if (status.includes("R") || status.includes("C")) {
      const destination = entries[++index];
      if (destination) names.push(destination);
    }
  }
  return names;
}

function readTrackedDiff(cwd, revision, pathspecs = []) {
  return readNulNames(cwd, ["diff", revision, "--name-only", "-z", "--", ...pathspecs]);
}

function readAmendFiles(cwd, includeWorkingTree = false) {
  if (includeWorkingTree) {
    try {
      return readTrackedDiff(cwd, "HEAD^");
    } catch {
      // `HEAD^` does not exist for a root commit. An amend still commits the
      // current root tree, so combine the committed tree with current changes.
      const committed = readNulNames(cwd, ["ls-tree", "-r", "--name-only", "-z", "HEAD"]);
      return [...new Set([...committed, ...readStatusNames(cwd)])];
    }
  }

  try {
    // Ordinary amend commits the index. Comparing the index with HEAD's
    // parent excludes unrelated unstaged working-tree edits.
    return readNulNames(cwd, ["diff", "--cached", "HEAD^", "--name-only", "-z"]);
  } catch {
    // `HEAD^` does not exist for a root commit. The index is still the
    // amended root tree, so compare it with the empty tree.
    return readNulNames(cwd, ["diff", "--cached", EMPTY_TREE, "--name-only", "-z"]);
  }
}

function readPathspecChanges(cwd, pathspecs) {
  try {
    return [
      ...readTrackedDiff(cwd, "HEAD", pathspecs),
      ...readNulNames(cwd, ["ls-files", "--others", "--exclude-standard", "-z", "--", ...pathspecs]),
    ];
  } catch {
    // A repository without a commit has no HEAD to diff. Explicit pathspecs
    // still provide a useful classification without mutating the index.
    return pathspecs;
  }
}

function readGitState(cwd, classification) {
  const stagedFiles = readNulNames(cwd, ["diff", "--staged", "--name-only", "-z"]);
  const commitFiles = classification.commitPathspecs.length > 0
    ? readNulNames(cwd, ["diff", "--staged", "--name-only", "-z", "--", ...classification.commitPathspecs])
    : stagedFiles;
  const state = { cwd, stagedFiles, commitFiles };

  if (classification.hasAmend) {
    try {
      state.commitFiles = readAmendFiles(cwd, classification.hasAutoStage);
    } catch {
      state.commitFiles = null;
    }
    return state;
  }

  if (classification.hasAutoStage) {
    try {
      state.commitFiles = readTrackedDiff(cwd, "HEAD", classification.commitPathspecs);
    } catch {
      state.commitFiles = readStatusNames(cwd);
    }
    return state;
  }

  if (classification.hasAddBefore) {
    const projected = new Set(stagedFiles);
    for (const addCommand of classification.addCommands) {
      const selection = addSelection(addCommand);
      if (!selection) {
        state.commitFiles = null;
        return state;
      }
      const names = selection.mode === "all"
        ? readStatusNames(cwd)
        : selection.mode === "tracked"
          ? (() => {
            try {
              return readTrackedDiff(cwd, "HEAD");
            } catch {
              return readStatusNames(cwd);
            }
          })()
          : readPathspecChanges(cwd, selection.pathspecs);
      for (const name of names) projected.add(name);
    }
    if (classification.commitPathspecs.length > 0) {
      const commitPaths = new Set(readPathspecChanges(cwd, classification.commitPathspecs));
      state.commitFiles = [...projected].filter((name) => commitPaths.has(name));
    } else {
      state.commitFiles = [...projected];
    }
  }
  return state;
}

export function decide(command, gitStateProvider) {
  const classification = classifyCommitCommand(command);
  if (!classification.isCommit) return null;

  let state;
  try {
    state = gitStateProvider(classification);
  } catch {
    return null;
  }
  const paths = state?.commitFiles;
  if (!Array.isArray(paths)) return null;

  const sensitive = paths.filter(isBehaviorSensitivePath);
  if (sensitive.length === 0) return null;
  if (paths.some(isDocumentationPath) || hasDocumentationExemption(command, { cwd: state.cwd })) return null;

  return {
    hookEventName: "PreToolUse",
    permissionDecision: "deny",
    permissionDecisionReason:
      "This commit contains behavior-sensitive changes but no documentation decision. " +
      "Stage the affected user/developer docs and CHANGELOG, or put `docs: none — <reason>` " +
      "in the commit message when documentation is genuinely unnecessary. " +
      `Detected ${sensitive.length} behavior-sensitive change${sensitive.length === 1 ? "" : "s"}.`,
  };
}

export function decideDocumentation({ command, gitStateProvider, ...legacyState }) {
  const provider = gitStateProvider ?? (() => ({
    cwd: process.cwd(),
    commitFiles: legacyState.commitFiles ?? legacyState.stagedFiles,
  }));
  return decide(command, provider);
}

function main() {
  let payload;
  try {
    const raw = readFileSync(0, "utf8");
    if (!raw.trim()) return;
    payload = JSON.parse(raw);
  } catch {
    return;
  }
  if (payload?.tool_name !== "Bash") return;
  try {
    const command = payload?.tool_input?.command ?? "";
    const classification = classifyCommitCommand(command);
    if (!classification.isCommit) return;
    const verdict = decide(command, () => readGitState(payload?.cwd || process.cwd(), classification));
    if (verdict) process.stdout.write(JSON.stringify({ hookSpecificOutput: verdict }));
  } catch {
    // A hook must not wedge a user's shell because git or the payload changed.
  }
}

function isMain() {
  if (!process.argv[1]) return false;
  try {
    return resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (isMain()) main();
