import { describe, it, expect } from "vitest";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import path from "node:path";

const root = path.resolve(__dirname, "../..");

function manifestVersions() {
  const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
  const tauri = JSON.parse(
    readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),
  );
  const cargo = readFileSync(path.join(root, "src-tauri", "Cargo.toml"), "utf8");
  const cargoVersion = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const lock = readFileSync(path.join(root, "src-tauri", "Cargo.lock"), "utf8");
  const lockVersion = lock.match(
    /^\[\[package\]\]\nname = "buildmesh"\nversion = "([^"]+)"/m,
  )?.[1];
  // npm stores the root version twice, and `npm install` rewrites both. A bump
  // that reaches only one of them reverts on the next install, which is how
  // the lockfile fell behind a release commit in the first place.
  const npmLock = JSON.parse(readFileSync(path.join(root, "package-lock.json"), "utf8"));
  return {
    pkg: pkg.version,
    tauri: tauri.version,
    cargo: cargoVersion,
    lock: lockVersion,
    npmLock: npmLock.version,
    npmLockRootEntry: npmLock.packages[""].version,
  };
}

function latestTagVersion(): string {
  // execFileSync, not execSync: a command string goes through cmd.exe on
  // Windows, which doubles the process starts (git's own run is ~75 ms; the
  // starts are what stall when the machine is saturated). The explicit timeout
  // turns a stalled start into a message that says so, instead of vitest's
  // generic "Test timed out".
  const tag = execFileSync("git", ["describe", "--abbrev=0", "--tags"], {
    cwd: root,
    encoding: "utf8",
    windowsHide: true,
    timeout: SUBPROCESS_TIMEOUT_MS - 5_000,
  }).trim();
  return tag.replace(/^v/, "");
}

const SEMVER = /^\d+\.\d+\.\d+(-[\w.-]+)?$/;

function parseSemver(v: string) {
  const m = v.match(/^(\d+)\.(\d+)\.(\d+)(?:-([\w.-]+))?$/);
  if (!m) throw new Error(`not semver: ${v}`);
  return {
    major: Number(m[1]),
    minor: Number(m[2]),
    patch: Number(m[3]),
    pre: m[4] ?? null,
  };
}

function gt(a: string, b: string) {
  const x = parseSemver(a);
  const y = parseSemver(b);
  const core =
    x.major !== y.major
      ? x.major - y.major
      : x.minor !== y.minor
        ? x.minor - y.minor
        : x.patch - y.patch;
  if (core !== 0) return core > 0;
  if (x.pre === null && y.pre === null) return false;
  if (x.pre === null) return true;
  if (y.pre === null) return false;
  return x.pre > y.pre;
}

// These tests shell out to `git describe` and to `scripts/set-version.mjs`,
// which are ~75ms on an idle machine but compete with the rest of the suite for
// CPU and have been observed taking ~50s alongside 287 other test files. They
// therefore declare a budget above the 30s suite default in `vitest.config.ts`
// (same class as issue #2049). The assertions themselves are unchanged — only
// how long they may take.
const SUBPROCESS_TIMEOUT_MS = 60000;

describe("app version manifests", () => {
  it("agree across every file that stores the version", () => {
    const versions = manifestVersions();
    // Every site must both report a version and report the same one: the
    // release gate compares them as strings, so an unreadable site is as
    // fatal as a disagreeing one.
    for (const [site, value] of Object.entries(versions)) {
      expect(value, `${site} reported no version`).toBeTruthy();
      expect(value, `${site} is ${value} but package.json is ${versions.pkg}`).toBe(
        versions.pkg,
      );
    }
  }, SUBPROCESS_TIMEOUT_MS);

  it("is valid semver with optional prerelease suffix", () => {
    const { pkg } = manifestVersions();
    expect(pkg).toMatch(SEMVER);
  }, SUBPROCESS_TIMEOUT_MS);

  it("is at least as new as the latest published release (no update prompt for local builds)", () => {
    const { pkg } = manifestVersions();
    const latestRelease = latestTagVersion();
    // Equal is allowed transiently (the stripped commit between tagging a
    // release and bumping back to the next -0 version), but the manifests
    // must never fall behind the published release.
    expect(gt(pkg, latestRelease) || pkg === latestRelease).toBe(true);
  }, SUBPROCESS_TIMEOUT_MS);

  // WiX ProductVersion is major.minor.patch[.build] with numeric-only fields
  // (each <= 65535). Tauri maps a semver prerelease into the 4th field, so
  // `1.3.0-dev` fails MSI bundling with "optional pre-release identifier in
  // app version must be numeric-only". The between-release marker is `-0`.
  it("prerelease identifier is MSI-safe (numeric-only, <= 65535)", () => {
    const { pkg } = manifestVersions();
    const { pre } = parseSemver(pkg);
    if (pre !== null) {
      expect(pre).toMatch(/^\d+$/);
      expect(Number(pre)).toBeLessThanOrEqual(65535);
    }
  }, SUBPROCESS_TIMEOUT_MS);

  it("between-release prerelease is sticky -0, not a counter", () => {
    const { pre } = parseSemver(manifestVersions().pkg);
    // Null is the transient stripped-for-tag window; otherwise the marker
    // stays at 0 until the next release. `-1` / `-dev` must not land.
    expect(pre === null || pre === "0").toBe(true);
  }, SUBPROCESS_TIMEOUT_MS);

  it("prerelease compares above its base's previous minor but below its own release", () => {
    expect(gt("1.3.0-0", "1.2.0")).toBe(true);
    expect(gt("1.3.0", "1.3.0-0")).toBe(true);
  }, SUBPROCESS_TIMEOUT_MS);
});

function runVersionSet(version: string): { status: number; stderr: string } {
  try {
    // process.execPath, no shell: one process start instead of cmd.exe plus node.
    execFileSync(process.execPath, ["scripts/set-version.mjs", version], {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    return { status: 0, stderr: "" };
  } catch (e: unknown) {
    const err = e as { status?: number; stderr?: string };
    return { status: err.status ?? 1, stderr: String(err.stderr ?? "") };
  }
}

describe("version:set", () => {
  it("rejects a non-numeric prerelease without writing manifests", () => {
    const before = manifestVersions();
    const result = runVersionSet("1.3.0-dev");
    expect(result.status).not.toBe(0);
    expect(result.stderr).toMatch(/MSI-safe/);
    expect(manifestVersions()).toEqual(before);
  }, SUBPROCESS_TIMEOUT_MS);

  it("rejects a prerelease above the WiX 65535 cap without writing manifests", () => {
    const before = manifestVersions();
    const result = runVersionSet("1.3.0-65536");
    expect(result.status).not.toBe(0);
    expect(result.stderr).toMatch(/MSI-safe/);
    expect(manifestVersions()).toEqual(before);
  }, SUBPROCESS_TIMEOUT_MS);
});

// The fanout is the fix for a release that bumped four of the five version
// sites, so it is tested against a real run of the real script rather than
// against its source. set-version.mjs resolves the repository root from its own
// path, so the test stages a throwaway tree with the layout it writes to and
// bumps that copy instead of the working tree.
const STAGED_FILES = [
  ["package.json"],
  ["package-lock.json"],
  ["src-tauri", "tauri.conf.json"],
  ["src-tauri", "Cargo.toml"],
  ["src-tauri", "Cargo.lock"],
] as const;

function stageManifestTree(): string {
  const dir = mkdtempSync(path.join(tmpdir(), "buildmesh-version-set-"));
  mkdirSync(path.join(dir, "scripts"), { recursive: true });
  mkdirSync(path.join(dir, "src-tauri"), { recursive: true });
  cpSync(
    path.join(root, "scripts", "set-version.mjs"),
    path.join(dir, "scripts", "set-version.mjs"),
  );
  for (const segments of STAGED_FILES) {
    cpSync(path.join(root, ...segments), path.join(dir, ...segments));
  }
  return dir;
}

function stagedVersions(dir: string) {
  const read = (segments: readonly string[]) =>
    readFileSync(path.join(dir, ...segments), "utf8");
  return {
    pkg: JSON.parse(read(["package.json"])).version,
    tauri: JSON.parse(read(["src-tauri", "tauri.conf.json"])).version,
    cargo: read(["src-tauri", "Cargo.toml"]).match(/^version\s*=\s*"([^"]+)"/m)?.[1],
    lock: read(["src-tauri", "Cargo.lock"]).match(
      /^\[\[package\]\]\nname = "buildmesh"\nversion = "([^"]+)"/m,
    )?.[1],
    npmLock: JSON.parse(read(["package-lock.json"])).version,
    npmLockRootEntry: JSON.parse(read(["package-lock.json"])).packages[""].version,
  };
}

function runStagedVersionSet(dir: string, version: string) {
  try {
    execFileSync(
      process.execPath,
      [path.join(dir, "scripts", "set-version.mjs"), version],
      { cwd: dir, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
    );
    return { status: 0, stderr: "" };
  } catch (e: unknown) {
    const err = e as { status?: number; stderr?: string };
    return { status: err.status ?? 1, stderr: String(err.stderr ?? "") };
  }
}

// Every pinned dependency version in the staged lockfile, so a test can prove
// none of them moved.
function dependencyVersions(dir: string): Record<string, unknown> {
  const lock = JSON.parse(readFileSync(path.join(dir, "package-lock.json"), "utf8"));
  const entries = Object.entries(lock.packages)
    .filter(([name]) => name !== "")
    .map(([name, meta]) => [name, (meta as { version?: unknown }).version]);
  return Object.fromEntries(entries);
}

describe("version:set fanout", () => {
  it("bumps every version site, including both in the lockfile", () => {
    const dir = stageManifestTree();
    try {
      const result = runStagedVersionSet(dir, "9.9.9-0");
      expect(result.status, result.stderr).toBe(0);
      const versions = stagedVersions(dir);
      for (const [site, value] of Object.entries(versions)) {
        expect(value, `${site} was not bumped`).toBe("9.9.9-0");
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  }, SUBPROCESS_TIMEOUT_MS);

  it("leaves the working tree alone", () => {
    const before = manifestVersions();
    const dir = stageManifestTree();
    try {
      runStagedVersionSet(dir, "9.9.9-0");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
    expect(manifestVersions()).toEqual(before);
  }, SUBPROCESS_TIMEOUT_MS);

  // The corruption path the unbounded search opened: with no version on the
  // `packages[""]` entry, a search over the rest of the file falls through to
  // the first dependency and rewrites *its* pinned version — a lockfile whose
  // resolved versions no longer match reality, which breaks `npm ci`, reported
  // as a successful bump. The second site is bounded to the `""` entry, so the
  // bump has to fail and write nothing.
  it("fails instead of rewriting a dependency when the root lockfile entry has no version", () => {
    const dir = stageManifestTree();
    try {
      const file = path.join(dir, "package-lock.json");
      const lock = JSON.parse(readFileSync(file, "utf8"));
      delete (lock.packages[""] as { version?: string }).version;
      writeFileSync(file, `${JSON.stringify(lock, null, 2)}\n`);
      const lockBefore = readFileSync(file, "utf8");
      const depsBefore = dependencyVersions(dir);

      const result = runStagedVersionSet(dir, "9.9.9-0");

      expect(result.status).not.toBe(0);
      expect(result.stderr).toMatch(/package-lock\.json/);
      // Nothing written: the failure happens before any file is written.
      expect(readFileSync(file, "utf8")).toBe(lockBefore);
      expect(dependencyVersions(dir)).toEqual(depsBefore);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  }, SUBPROCESS_TIMEOUT_MS);

  it("fails rather than half-updating when the lockfile has no packages map", () => {
    const dir = stageManifestTree();
    try {
      // lockfileVersion 1 shape: a top-level version and no `packages` map, so
      // the second site does not exist to be written.
      const file = path.join(dir, "package-lock.json");
      writeFileSync(
        file,
        `${JSON.stringify({ name: "app", version: "1.3.0", lockfileVersion: 1 }, null, 2)}\n`,
      );
      const before = readFileSync(file, "utf8");
      const result = runStagedVersionSet(dir, "9.9.9-0");
      expect(result.status).not.toBe(0);
      expect(result.stderr).toMatch(/package-lock\.json/);
      // Not even the top-level mirror moves: the target is rejected whole.
      expect(readFileSync(file, "utf8")).toBe(before);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  }, SUBPROCESS_TIMEOUT_MS);
});
