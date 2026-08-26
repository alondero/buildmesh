import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+(-[\w.-]+)?$/.test(version)) {
  console.error("Usage: npm run version:set -- <semver>   e.g. 1.3.0-dev or 1.3.0");
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
];

const JSON_VERSION = /^(\s*"version"\s*:\s*")[^"]*(")/m;
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
