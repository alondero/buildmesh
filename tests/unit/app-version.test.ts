import { describe, it, expect } from "vitest";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { execSync, execFileSync } from "node:child_process";
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
  const tag = execSync("git describe --abbrev=0 --tags", { cwd: root })
    .toString()
    .trim();
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
  });

  it("is valid semver with optional prerelease suffix", () => {
    const { pkg } = manifestVersions();
    expect(pkg).toMatch(SEMVER);
  });

  it("is at least as new as the latest published release (no update prompt for local builds)", () => {
    const { pkg } = manifestVersions();
    const latestRelease = latestTagVersion();
    // Equal is allowed transiently (the stripped commit between tagging a
    // release and bumping back to the next -0 version), but the manifests
    // must never fall behind the published release.
    expect(gt(pkg, latestRelease) || pkg === latestRelease).toBe(true);
  });

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
  });

  it("between-release prerelease is sticky -0, not a counter", () => {
    const { pre } = parseSemver(manifestVersions().pkg);
    // Null is the transient stripped-for-tag window; otherwise the marker
    // stays at 0 until the next release. `-1` / `-dev` must not land.
    expect(pre === null || pre === "0").toBe(true);
  });

  it("prerelease compares above its base's previous minor but below its own release", () => {
    expect(gt("1.3.0-0", "1.2.0")).toBe(true);
    expect(gt("1.3.0", "1.3.0-0")).toBe(true);
  });
});

function runVersionSet(version: string): { status: number; stderr: string } {
  try {
    execSync(`node scripts/set-version.mjs ${version}`, {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
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
  });

  it("rejects a prerelease above the WiX 65535 cap without writing manifests", () => {
    const before = manifestVersions();
    const result = runVersionSet("1.3.0-65536");
    expect(result.status).not.toBe(0);
    expect(result.stderr).toMatch(/MSI-safe/);
    expect(manifestVersions()).toEqual(before);
  });
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

describe("version:set fanout", () => {
  it("bumps every version site, including both in the lockfile", () => {
    const dir = stageManifestTree();
    try {
      execFileSync(
        process.execPath,
        [path.join(dir, "scripts", "set-version.mjs"), "9.9.9-0"],
        { cwd: dir, encoding: "utf8" },
      );
      const versions = stagedVersions(dir);
      for (const [site, value] of Object.entries(versions)) {
        expect(value, `${site} was not bumped`).toBe("9.9.9-0");
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("leaves the working tree alone", () => {
    const before = manifestVersions();
    const dir = stageManifestTree();
    try {
      execFileSync(
        process.execPath,
        [path.join(dir, "scripts", "set-version.mjs"), "9.9.9-0"],
        { cwd: dir, encoding: "utf8" },
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
    expect(manifestVersions()).toEqual(before);
  });
});
