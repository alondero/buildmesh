// Parser cases for the git2 seam guard. The tree walk lives in
// tests/agent-infra/git2-ownership.test.mjs so a Rust-only edit can still
// reuse the frontend test suite.

import { describe, it, expect } from "vitest";
import { stripCfgTestItems } from "../agent-infra/git2-ownership.mjs";

describe("git2 ownership", () => {
  it("strips a #[cfg(test)] module but keeps production use above it", () => {
    const source = [
      "fn open() { let _ = git2::Repository::open(path); }",
      "#[cfg(test)]",
      "mod tests {",
      "    fn fixture() { let _ = git2::Repository::init(path); }",
      "}",
      "fn after() {}",
    ].join("\n");
    const stripped = stripCfgTestItems(source);
    expect(stripped).toContain("fn open()");
    expect(stripped).toContain("fn after()");
    expect(stripped).not.toContain("Repository::init");
  });

  it("does not strip #[cfg(not(test))]", () => {
    const source = "#[cfg(not(test))]\nfn open() { git2::Repository::open(path); }\n";
    expect(stripCfgTestItems(source)).toContain("git2::Repository::open");
  });

  it("keeps production code after a #[cfg(test)] item whose braces never balance", () => {
    const source = [
      "fn before() { let _ = git2::Repository::open(path); }",
      "#[cfg(test)]",
      "mod launch_migration_tests {",
      '    fn t() { assert_eq!(raw, "{invalid"); }',
      "}",
      "pub fn create_agent_node() { let _ = git2::Repository::open(path); }",
    ].join("\n");
    const stripped = stripCfgTestItems(source);
    expect(stripped).toContain("fn before()");
    expect(stripped).toContain("pub fn create_agent_node");
  });

  it("does not drop the production line after an inline #[cfg(test)] item", () => {
    const source = [
      "#[cfg(test)] fn helper() { let _ = git2::Repository::init(path); }",
      "fn open() { let _ = git2::Repository::open(path); }",
    ].join("\n");
    const stripped = stripCfgTestItems(source);
    expect(stripped).toContain("fn open()");
    expect(stripped).not.toContain("Repository::init");
  });

  it("strips #[cfg(any)] and #[cfg(all)] fixtures, including a paren inside a feature string", () => {
    const source = [
      '#[cfg(any(test, feature = "x"))]',
      "mod a { fn f() { let _ = git2::Repository::init(path); } }",
      '#[cfg(all(feature = "f(x)", test))]',
      "mod b { fn g() { let _ = git2::Repository::init(path); } }",
      "#[cfg(all(not(test)))]",
      "fn kept() { let _ = git2::Repository::open(path); }",
    ].join("\n");
    const stripped = stripCfgTestItems(source);
    expect(stripped).toContain("fn kept()");
    expect(stripped).not.toContain("Repository::init");
  });
});
