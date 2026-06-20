import { describe, it, expect } from "vitest";
import {
  checkContentViolations,
  checkWorktreeEscape,
  collectNewText,
} from "../../.claude/hooks/guard-antipatterns.mjs";

// Real-world paths from this repo's worktree layout (see CLAUDE.local.md).
const CWD_WORKTREE = "X:\\src\\buildmesh\\.claude\\worktrees\\red-rare-hedge";
const CWD_MAIN = "X:\\src\\buildmesh";

describe("checkWorktreeEscape", () => {
  it("blocks an absolute edit into the main checkout while cwd is a worktree", () => {
    const msg = checkWorktreeEscape(
      "X:\\src\\buildmesh\\src-tauri\\src\\db\\mod.rs",
      CWD_WORKTREE,
    );
    expect(msg).toContain("[worktree-escape]");
  });

  it("blocks editing the main checkout's CLAUDE.md from a worktree", () => {
    const msg = checkWorktreeEscape("X:\\src\\buildmesh\\CLAUDE.md", CWD_WORKTREE);
    expect(msg).toContain("[worktree-escape]");
  });

  it("allows an edit inside the worktree", () => {
    const msg = checkWorktreeEscape(
      "X:\\src\\buildmesh\\.claude\\worktrees\\red-rare-hedge\\src-tauri\\src\\db\\mod.rs",
      CWD_WORKTREE,
    );
    expect(msg).toBeNull();
  });

  it("blocks editing a DIFFERENT worktree from this one", () => {
    const msg = checkWorktreeEscape(
      "X:\\src\\buildmesh\\.claude\\worktrees\\other-tree\\src\\App.tsx",
      CWD_WORKTREE,
    );
    expect(msg).toContain("[worktree-escape]");
  });

  it("is a no-op when cwd is the main checkout (not a worktree)", () => {
    const msg = checkWorktreeEscape(
      "X:\\src\\buildmesh\\src-tauri\\src\\db\\mod.rs",
      CWD_MAIN,
    );
    expect(msg).toBeNull();
  });

  it("ignores relative paths (they resolve against the worktree cwd)", () => {
    expect(checkWorktreeEscape("src-tauri/src/db/mod.rs", CWD_WORKTREE)).toBeNull();
  });

  it("ignores paths on a different drive (e.g. the memory store)", () => {
    const msg = checkWorktreeEscape(
      "C:\\Users\\alond\\.claude\\projects\\X--src-buildmesh\\memory\\note.md",
      CWD_WORKTREE,
    );
    expect(msg).toBeNull();
  });

  it("honours the BUILDMESH_ALLOW_WORKTREE_ESCAPE escape hatch", () => {
    const prev = process.env.BUILDMESH_ALLOW_WORKTREE_ESCAPE;
    process.env.BUILDMESH_ALLOW_WORKTREE_ESCAPE = "1";
    try {
      expect(
        checkWorktreeEscape("X:\\src\\buildmesh\\CLAUDE.md", CWD_WORKTREE),
      ).toBeNull();
    } finally {
      if (prev === undefined) delete process.env.BUILDMESH_ALLOW_WORKTREE_ESCAPE;
      else process.env.BUILDMESH_ALLOW_WORKTREE_ESCAPE = prev;
    }
  });
});

describe("checkContentViolations (regression — existing rules still fire)", () => {
  it("blocks .dispose() in a Terminal component", () => {
    const v = checkContentViolations("src/components/Terminal.tsx", "term.dispose();");
    expect(v.join()).toContain("terminal-dispose");
  });

  it("allows .dispose() with the escape-hatch comment", () => {
    const v = checkContentViolations(
      "src/components/Terminal.tsx",
      "term.dispose(); // allow-dispose",
    );
    expect(v).toHaveLength(0);
  });

  it("blocks a hand-built \\\\wsl$ path in a Rust file outside env/", () => {
    const v = checkContentViolations(
      "src-tauri/src/agent/spawn.rs",
      'let p = format!("\\\\\\\\wsl$\\\\{}", distro);',
    );
    expect(v.join()).toContain("wsl-path-outside-env");
  });

  it("allows \\\\wsl$ inside src-tauri/src/env/", () => {
    const v = checkContentViolations(
      "src-tauri/src/env/mod.rs",
      'let p = format!("\\\\\\\\wsl$\\\\{}", distro);',
    );
    expect(v).toHaveLength(0);
  });
});

describe("collectNewText", () => {
  it("reads Write content, Edit new_string, and MultiEdit edits", () => {
    expect(collectNewText({ content: "a" })).toBe("a");
    expect(collectNewText({ new_string: "b" })).toBe("b");
    expect(collectNewText({ edits: [{ new_string: "c" }, { new_string: "d" }] })).toBe(
      "c\nd",
    );
  });
});
