import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2];
const parsed = version?.match(/^(\d+\.\d+\.\d+)(?:-(.+))?$/);
if (!parsed) {
  console.error("Usage: npm run version:set -- <semver>   e.g. 1.3.0-0 or 1.3.0");
  process.exit(1);
}
// WiX MSI requires a numeric-only prerelease identifier <= 65535. `-dev`
// is valid semver but fails the bundler; the between-release marker is `-0`.
if (parsed[2] !== undefined && (!/^\d+$/.test(parsed[2]) || Number(parsed[2]) > 65535)) {
  console.error(
    `Prerelease "${parsed[2]}" is not MSI-safe (must be numeric-only and <= 65535). Use e.g. 1.3.0-0`,
  );
  process.exit(1);
}

const targets = [
  { file: "package.json", apply: applyJson },
  { file: path.join("src-tauri", "tauri.conf.json"), apply: applyJson },
  { file: path.join("src-tauri", "Cargo.toml"), apply: (t) => applyTomlBlock(t, "[package]") },
  {
    file: path.join("src-tauri", "Cargo.lock"),
    apply: (t) => applyTomlNamedPackage(t, "buildmesh"),
  },
  // The lockfile is a manifest too, not just build output: npm keeps the root
  // version in it and rewrites it on the next install, so a bump that skips
  // this file leaves the tree one install away from an unrelated diff — which
  // is how a release commit once bumped four of the five files. See
  // docs/development/releasing.md.
  { file: "package-lock.json", apply: applyLockJson },
];

const JSON_VERSION = /^(\s*"version"\s*:\s*")[^"]*(")/m;
// The root package entry inside the lockfile's `packages` map. The version
// that follows it is the second of the two places the root version lives.
const LOCK_ROOT_ENTRY = /"packages"\s*:\s*\{\s*""\s*:\s*\{/;
const TOML_VERSION = /^version\s*=\s*"[^"]*"\s*$/;

const updates = [];
for (const target of targets) {
  const full = path.join(root, target.file);
  const text = readFileSync(full, "utf8");
  const updated = target.apply(text);
  if (updated === null) {
    console.error(`Failed to update ${target.file} — version pattern not found`);
    process.exit(1);
  }
  updates.push({ full, updated });
}

for (const { full, updated } of updates) {
  writeFileSync(full, updated);
  console.log(`updated ${path.relative(root, full)} -> ${version}`);
}

function applyJson(text) {
  if (!JSON_VERSION.test(text)) return null;
  return text.replace(JSON_VERSION, `$1${version}$2`);
}

// The lockfile carries the root version in two places: the top-level mirror
// npm writes from package.json, and the `packages[""]` entry. Both move
// together — patching only the top level leaves the next `npm install` to
// rewrite the file and put the drift straight back. A lockfile without a
// `packages` map (lockfileVersion 1) has no second site and is rejected
// rather than half-updated.
function applyLockJson(text) {
  const topLevel = applyJson(text);
  if (topLevel === null) return null;
  const entry = LOCK_ROOT_ENTRY.exec(topLevel);
  if (!entry) return null;
  const start = entry.index + entry[0].length;
  const rootEntry = topLevel.slice(start);
  if (!JSON_VERSION.test(rootEntry)) return null;
  return topLevel.slice(0, start) + rootEntry.replace(JSON_VERSION, `$1${version}$2`);
}

function applyTomlBlock(text, header) {
  let inBlock = false;
  let replaced = false;
  const out = text.split("\n").map((line) => {
    if (line.startsWith("[")) inBlock = line.startsWith(header);
    else if (inBlock && TOML_VERSION.test(line)) {
      replaced = true;
      return `version = "${version}"`;
    }
    return line;
  });
  return replaced ? out.join("\n") : null;
}

function applyTomlNamedPackage(text, name) {
  let inTarget = false;
  let replaced = false;
  const out = text.split("\n").map((line) => {
    if (/^\[\[package\]\]/.test(line)) inTarget = false;
    else if (new RegExp(`^name\\s*=\\s*"${name}"\\s*$`).test(line)) inTarget = true;
    else if (inTarget && TOML_VERSION.test(line)) {
      replaced = true;
      return `version = "${version}"`;
    }
    return line;
  });
  return replaced ? out.join("\n") : null;
}
