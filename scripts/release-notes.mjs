#!/usr/bin/env node
// Release-note generator.
//
// Builds a draft docs/releases/vX.Y.Z.md from the Conventional Commits merged
// since the previous release tag. The versioned note is authored once at
// release time instead of being edited by every pull request — a single shared
// release-note file conflicts on nearly every pair of parallel PRs.
//
// Usage:
//   npm run release:notes                       preview the draft on stdout
//   npm run release:notes -- --write            write docs/releases/v<version>.md
//   npm run release:notes -- 1.5.0 --base v1.4.0
//   npm run release:notes -- --all              include internal commits too
//
// The version defaults to the manifest version with any prerelease suffix
// stripped (the `-0` between-release marker); the base defaults to the most
// recent `vX.Y.Z` tag reachable from HEAD. The output is a skeleton to curate,
// not a finished note — see docs/releases/README.md.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const releasesDir = path.join(root, "docs", "releases");

// Commit types that describe user-visible behavior, and the ones kept out of a
// release note. Anything else is surfaced under "Other" so it is never dropped
// silently.
const USER_TYPES = { feat: "Features", fix: "Fixes", perf: "Performance", revert: "Reverts" };
const INTERNAL_TYPES = {
  refactor: "Refactors",
  docs: "Documentation",
  test: "Tests",
  tests: "Tests",
  ci: "CI",
  build: "Build",
  style: "Style",
  chore: "Chores",
  deps: "Dependencies",
};
const ORDER = [
  "Breaking changes",
  "Features",
  "Fixes",
  "Performance",
  "Reverts",
  "Other",
  ...[...new Set(Object.values(INTERNAL_TYPES))],
];

// A squash-merged commit whose subject is only a co-author trailer (`@ (#1600)`)
// keeps its real subject on the first body line.
function firstBodyLine(bodyText) {
  const line = bodyText
    .split("\n")
    .map((entry) => entry.trim())
    .find((entry) => entry.length > 0 && !/^@/.test(entry));
  return line ?? null;
}

export function parseConventionalCommit(subject, body = "") {
  const bodyText = String(body ?? "");
  let text = String(subject ?? "").trim();
  // Some squash merges leave a bare co-author trailer as the subject; the real
  // subject is the first body line, so promote it rather than rendering `@`.
  if (/^@/.test(text) || text.length === 0) {
    const promoted = firstBodyLine(bodyText);
    if (promoted) {
      // Keep the PR reference the discarded subject carried, so promoting the
      // real subject does not silently drop the link.
      const carried = text.match(/((?:\(#\d+\)\s*)+)$/)?.[1] ?? "";
      text = carried ? `${promoted} ${carried.trim()}` : promoted;
    }
  }
  const match = text.match(/^([a-zA-Z]+)(?:\(([^)]*)\))?(!)?:\s*(.+)$/);
  const breaking = Boolean(match?.[3]) || /(?:^|\n)BREAKING[ -]CHANGE:/.test(bodyText);
  const description = (match ? match[4] : text).trim();
  // GitHub squash-merge subjects end with the PR number; lift it out so the
  // rendered entry does not repeat it. Stacked PRs leave several numbers
  // behind (`... (#1939) (#1940)`), so strip every trailing reference and keep
  // the last one — the PR that actually landed.
  const trailing = description.match(/^(.*?)\s*(?:\(#(\d+)\)\s*)+$/);
  const visible = trailing ? trailing[1].trim() : description;
  // Only an explicit `Closes #N` trailer counts. A bare `#N` in a body is
  // usually an *issue* or an unrelated cross-reference, and publishing it as a
  // PR link is worse than omitting the number.
  const trailer = bodyText.match(/(?:^|\n)(?:Closes|Fixes|Resolves)\s+#(\d+)/i)?.[1] ?? null;
  // ...but do not repeat a number the subject already cites, such as
  // `add Muse transcript reader (issue #1708)` with a matching `Closes #1708`.
  const pr = trailing?.[2] ?? (trailer && !visible.includes(`#${trailer}`) ? trailer : null);
  if (!match) return { type: null, scope: null, breaking, subject: visible, pr };
  return { type: match[1].toLowerCase(), scope: match[2] || null, breaking, subject: visible, pr };
}

export function isInternal(commit) {
  return !commit.breaking && !USER_TYPES[commit.type] && Boolean(INTERNAL_TYPES[commit.type]);
}

function categoryFor(commit) {
  if (commit.breaking) return "Breaking changes";
  return USER_TYPES[commit.type] ?? INTERNAL_TYPES[commit.type] ?? "Other";
}

export function groupCommits(commits) {
  const groups = new Map();
  for (const commit of commits) {
    const category = categoryFor(commit);
    if (!groups.has(category)) groups.set(category, []);
    groups.get(category).push(commit);
  }
  return ORDER.filter((category) => groups.has(category)).map((category) => ({
    title: category,
    commits: groups.get(category),
  }));
}

function entry(commit) {
  return `- ${commit.subject}${commit.pr ? ` (#${commit.pr})` : ""}`;
}

function reference(commit) {
  const scope = commit.scope ? `(${commit.scope})` : "";
  const type = commit.type ?? "other";
  return `${type}${scope}: ${commit.subject}${commit.pr ? ` (#${commit.pr})` : ""}`;
}

export function renderReleaseNotes({ version, base, commits, includeInternal = false }) {
  const visible = includeInternal ? commits : commits.filter((commit) => !isInternal(commit));
  const hidden = includeInternal ? [] : commits.filter(isInternal);
  const count = commits.length;
  const lines = [
    `# Buildmesh v${version}`,
    "",
    "<!-- Audience: Buildmesh users. Lifecycle: draft before the matching tag is published; historical release record afterward. -->",
    "",
    "_Draft — not yet released._",
    "",
    `<!-- Generated from ${count} commit${count === 1 ? "" : "s"} since ${base}.`,
    "     Curate before publishing: keep the user-visible entries, drop internal",
    "     work, and add the Highlights and Upgrade notes a reader needs. -->",
    "",
  ];

  const groups = groupCommits(visible);
  if (groups.length === 0) {
    lines.push("_No user-visible commits found in this range._", "");
  }
  for (const group of groups) {
    lines.push(`## ${group.title}`, "");
    for (const commit of group.commits) lines.push(entry(commit));
    lines.push("");
  }

  if (hidden.length > 0) {
    lines.push("<!-- Internal commits excluded above (not user-visible):");
    for (const commit of hidden) lines.push(`     - ${reference(commit)}`);
    lines.push("-->", "");
  }

  return `${lines.join("\n").replace(/\n+$/, "")}\n`;
}

// Version-aware comparison so v1.10.0 sorts above v1.9.0.
export function compareVersions(a, b) {
  const pa = String(a).replace(/^v/, "").split(".").map((part) => Number.parseInt(part, 10) || 0);
  const pb = String(b).replace(/^v/, "").split(".").map((part) => Number.parseInt(part, 10) || 0);
  for (let index = 0; index < 3; index += 1) {
    const diff = (pa[index] ?? 0) - (pb[index] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

// Pick the base for the next release.
//
// Release tags in this repo point at the pre-merge commit on the release
// branch, so `git describe` walks back to v1.0.0 — the only tag actually
// reachable from main. That silently inflated a draft to 613 commits when the
// real range is 95. Instead, take the highest-versioned tag BELOW the release
// being prepared and map it onto the `chore(release): vX.Y.Z` commit that
// carries the same version into main, since that commit is the true boundary
// on main. Falls back to plain reachability only when no such commit exists.
export function resolveBase({ version, tags, releaseCommits, reachableTag }) {
  const target = String(version).replace(/^v/, "");
  const priorTags = tags
    .filter((tag) => /^v\d+\.\d+\.\d+$/.test(tag) && compareVersions(tag, target) < 0)
    .sort((a, b) => compareVersions(b, a));

  for (const tag of priorTags) {
    const match = releaseCommits.find((commit) => commit.version === tag);
    if (match) return { ref: match.commit, source: `release-commit for ${tag}` };
  }

  // No release commit carried the tag into main; fall back to a tag that is
  // actually reachable from HEAD, then to the most recent one either way.
  if (reachableTag) return { ref: reachableTag, source: "reachable tag" };
  return { ref: priorTags[0] ?? tags[0] ?? null, source: "highest prior tag" };
}

function git(args) {
  return execFileSync("git", args, { cwd: root, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }).trim();
}

function manifestVersion() {
  const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
  return String(pkg.version ?? "").replace(/-.*$/, "");
}

// `chore(release): vX.Y.Z` commits reachable from HEAD, newest first. These
// are the on-main boundaries for each shipped version.
function releaseCommits() {
  let raw = "";
  try {
    raw = git(["log", "--first-parent", "--format=%s%x1f%H", "HEAD"]);
  } catch {
    return [];
  }
  const found = [];
  for (const line of raw.split("\n")) {
    const [subject, hash] = line.split("\x1f");
    const match = String(subject ?? "").match(/^chore\(release\): v(\d+\.\d+\.\d+)/);
    if (match && hash) found.push({ version: `v${match[1]}`, commit: hash });
  }
  return found;
}

function versionTags() {
  try {
    return git(["tag", "--list", "v*"]).split("\n").filter(Boolean);
  } catch {
    return [];
  }
}

function reachableTag() {
  try {
    return git(["describe", "--tags", "--abbrev=0", "--match", "v[0-9]*"]);
  } catch {
    return null;
  }
}

function rootCommit() {
  try {
    // [0] is the first root; `.pop()` would pick an arbitrary one when a
    // grafted or merged history has several.
    return git(["rev-list", "--max-parents=0", "HEAD"]).split("\n")[0];
  } catch {
    return null;
  }
}

function resolvePreviousBase(version) {
  const resolved = resolveBase({
    version,
    tags: versionTags(),
    releaseCommits: releaseCommits(),
    reachableTag: reachableTag(),
  });
  if (resolved.ref) return resolved;
  const root = rootCommit();
  return root
    ? { ref: root, source: "the first commit" }
    : { ref: null, source: "the start of history" };
}

function readCommits(base) {
  const range = base ? `${base}..HEAD` : "HEAD";
  const raw = execFileSync("git", ["log", "--no-merges", "--format=%s%x1f%b%x1e", range], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  return raw
    .split("\x1e")
    .map((record) => record.trim())
    .filter(Boolean)
    .map((record) => {
      const [subject, body = ""] = record.split("\x1f");
      return parseConventionalCommit(subject, body);
    });
}

function printUsage() {
  console.log(`Usage: npm run release:notes -- [version] [options]

  version            Release version (default: manifest version without the -0 suffix)
  --base <ref>       Commit/tag to start from (default: the boundary of the highest
                     released version below this one)
  --write            Write docs/releases/v<version>.md instead of printing it
  --force            Overwrite an existing release-note file
  --all              Include internal commits (chore, refactor, docs, ...)
  -h, --help         Show this message`);
}

function main() {
  const args = process.argv.slice(2);
  let version = null;
  let base = null;
  let write = false;
  let force = false;
  let includeInternal = false;

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--write") write = true;
    else if (arg === "--force") force = true;
    else if (arg === "--all") includeInternal = true;
    else if (arg === "--base") {
      const value = args[++index];
      // A missing value would silently fall back to the auto-detected base and
      // produce a wrong-range note, so fail loudly instead.
      if (!value || value.startsWith("-")) {
        console.error(`--base needs a commit or tag.`);
        process.exitCode = 2;
        return;
      }
      base = value;
    }
    else if (arg.startsWith("--base=")) base = arg.slice("--base=".length);
    else if (arg === "-h" || arg === "--help") {
      printUsage();
      return;
    } else if (arg.startsWith("-") && arg !== "-") {
      console.error(`Unknown option: ${arg}\n`);
      printUsage();
      process.exitCode = 2;
      return;
    } else {
      version = arg;
    }
  }

  version = String(version ?? manifestVersion()).replace(/^v/, "");
  if (!/^\d+\.\d+\.\d+$/.test(version)) {
    console.error(`Invalid release version "${version}" — expected X.Y.Z.`);
    process.exitCode = 1;
    return;
  }

  const resolved = base
    ? { ref: base, source: base }
    : resolvePreviousBase(version);
  const commits = readCommits(resolved.ref);
  const markdown = renderReleaseNotes({
    version,
    base: resolved.ref ?? "the start of history",
    commits,
    includeInternal,
  });

  if (!write) {
    process.stdout.write(markdown);
    return;
  }

  const target = path.join(releasesDir, `v${version}.md`);
  if (existsSync(target) && !force) {
    console.error(
      `${path.relative(root, target)} already exists — it is the active draft, so curate it in place.\n`
      + `Preview with: npm run release:notes\n`
      + `Regenerate over it with: npm run release:notes -- --write --force`,
    );
    process.exitCode = 1;
    return;
  }
  writeFileSync(target, markdown);
  console.log(`wrote ${path.relative(root, target)} (${commits.length} commits since ${resolved.ref ?? "the start of history"}, via ${resolved.source})`);
}

const invokedDirectly = process.argv[1]
  && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) main();
