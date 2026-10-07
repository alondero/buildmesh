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
// Where the lockfile's dependency map begins, so the top-level mirror is only
// ever searched in the head above it.
const LOCK_PACKAGES_KEY = '"packages"';
// The root package entry inside that map. Group 1 is the key's own
// indentation, which is also the column its closing brace sits at — the two
// together bound the entry so a search inside it cannot escape into the
// dependency that follows.
const LOCK_ROOT_ENTRY = /"packages"\s*:\s*\{\s*\n([ ]*)""\s*:\s*\{/;
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
// rewrite the file and put the drift straight back.
//
// Both searches are *bounded* to the site they own: the mirror before the
// `"packages"` key, the second site inside the `""` entry (delimited by that
// entry's own closing brace). An unbounded search over the rest of the file
// finds the first dependency's `"version"` instead and rewrites a pinned
// dependency — silent corruption of the file that breaks `npm ci`, reported as
// a successful bump. Every shape this cannot find a root version in (no
// `packages` map, no `""` entry, no closing brace, no version key) returns null
// so the caller exits non-zero before writing anything.
function applyLockJson(text) {
  const packagesAt = text.indexOf(LOCK_PACKAGES_KEY);
  if (packagesAt === -1) return null;
  const entry = LOCK_ROOT_ENTRY.exec(text);
  if (!entry) return null;
  const mirror = text.slice(0, packagesAt);
  const start = entry.index + entry[0].length;
  const end = text.indexOf(`\n${entry[1]}}`, start);
  if (end === -1) return null;
  const rootEntry = text.slice(start, end);
  if (!JSON_VERSION.test(mirror) || !JSON_VERSION.test(rootEntry)) return null;
  return (
    mirror.replace(JSON_VERSION, `$1${version}$2`) +
    text.slice(packagesAt, start) +
    rootEntry.replace(JSON_VERSION, `$1${version}$2`) +
    text.slice(end)
  );
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
