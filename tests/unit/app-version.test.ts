import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { execSync } from "node:child_process";
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
  return { pkg: pkg.version, tauri: tauri.version, cargo: cargoVersion, lock: lockVersion };
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
  it("agree across package.json, tauri.conf.json, Cargo.toml and Cargo.lock", () => {
    const versions = manifestVersions();
    expect(versions.pkg).toBeTruthy();
    expect(versions.cargo).toBeTruthy();
    expect(versions.lock).toBeTruthy();
    expect(versions.pkg).toBe(versions.tauri);
    expect(versions.pkg).toBe(versions.cargo);
    expect(versions.pkg).toBe(versions.lock);
  });

  it("is valid semver with optional prerelease suffix", () => {
    const { pkg } = manifestVersions();
    expect(pkg).toMatch(SEMVER);
  });

  it("is at least as new as the latest published release (no update prompt for local builds)", () => {
    const { pkg } = manifestVersions();
    const latestRelease = latestTagVersion();
    // Equal is allowed transiently (the stripped commit between tagging a
    // release and bumping back to the next -dev version), but the manifests
    // must never fall behind the published release.
    expect(gt(pkg, latestRelease) || pkg === latestRelease).toBe(true);
  });

  it("prerelease compares above its base's previous minor but below its own release", () => {
    expect(gt("1.3.0-dev", "1.2.0")).toBe(true);
    expect(gt("1.3.0", "1.3.0-dev")).toBe(true);
  });
});
