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

// Issue #733 — bare `rounded` in src/components bypasses the `--radius-md`
// token. The guard must block new bare-rounded writes while preserving the
// escape hatch and the legitimate size-suffixed variants.
describe("checkContentViolations — bare-rounded rules (#733)", () => {
  it("blocks bare `rounded` in a component className", () => {
    const v = checkContentViolations(
      "src/components/Sidebar/MeshItem.tsx",
      'className="flex items-center gap-2 px-2 py-1 rounded text-xs bg-bg-overlay"',
    );
    expect(v.join()).toContain("bare-rounded-in-component");
  });

  it("blocks bare `rounded-r` (bare directional) in a component className", () => {
    const v = checkContentViolations(
      "src/components/Sidebar/Pagination.tsx",
      'className="px-2 py-1 rounded-r text-xs bg-bg-card"',
    );
    expect(v.join()).toContain("bare-rounded-directional-in-component");
  });

  it("allows `rounded-md` in a component className (already token-bound)", () => {
    const v = checkContentViolations(
      "src/components/Sidebar/MeshItem.tsx",
      'className="flex items-center gap-2 px-2 py-1 rounded-md text-xs bg-bg-overlay"',
    );
    expect(v).toHaveLength(0);
  });

  it("allows `rounded-r-md` in a component className (directional already sized)", () => {
    const v = checkContentViolations(
      "src/components/Sidebar/Pagination.tsx",
      'className="px-2 py-1 rounded-r-md text-xs bg-bg-card"',
    );
    expect(v).toHaveLength(0);
  });

  it("allows `rounded-r-[6px]` in a component className (arbitrary value)", () => {
    // The bare rule already allows `rounded-[6px]` arbitrary values, so
    // the directional rule must too — consistency check.
    const v = checkContentViolations(
      "src/components/Sidebar/Pagination.tsx",
      'className="px-2 py-1 rounded-r-[6px] text-xs bg-bg-card"',
    );
    expect(v).toHaveLength(0);
  });

  it("allows the full set of token-bound rounded sizes", () => {
    const sizes = ["sm", "md", "lg", "xl", "2xl", "3xl", "full", "none", "pill"];
    for (const size of sizes) {
      const v = checkContentViolations(
        "src/components/Sidebar/MeshItem.tsx",
        `className="p-2 rounded-${size}"`,
      );
      expect(v, `rounded-${size} should not be flagged`).toHaveLength(0);
    }
  });

  it("allows bare `rounded` with the `// allow-bare-rounded` escape comment", () => {
    const v = checkContentViolations(
      "src/components/Probe/RepositoryTab.tsx",
      'className={`px-1 py-px rounded text-[9px] ${color}`} // allow-bare-rounded — 9px status badge',
    );
    expect(v).toHaveLength(0);
  });

  it("allows bare `rounded-r` with the `// allow-bare-rounded` escape comment", () => {
    const v = checkContentViolations(
      "src/components/Sidebar/Pagination.tsx",
      'className="px-2 py-1 rounded-r text-xs" // allow-bare-rounded',
    );
    expect(v).toHaveLength(0);
  });

  it("does NOT police bare `rounded` outside src/components (mobile + Rust)", () => {
    // The bug is specific to the React component layer where Tailwind reads
    // the --radius-md token; mobile uses inline styles and Rust is unrelated.
    const mobile = checkContentViolations(
      "src/mobile/screens/NodeList.tsx",
      "borderRadius: 4,",
    );
    expect(mobile).toHaveLength(0);

    const rust = checkContentViolations(
      "src-tauri/src/agent/spawn.rs",
      'let radius = "rounded";',
    );
    expect(rust).toHaveLength(0);
  });
});

// Issue #1982 — PowerShell evaluates a command's OUTPUT, not its exit code, so a
// silent native command used as a bare condition is always false. `if (git
// merge-base --is-ancestor …)` printed the empty string on success and took the
// else branch on every call: the check could never report "YES".
describe("checkContentViolations — PowerShell native-command conditions (#1982)", () => {
  it("blocks `if (git merge-base --is-ancestor ...)` in a PowerShell script", () => {
    const v = checkContentViolations(
      "scripts/check-merge.ps1",
      'if (git merge-base --is-ancestor $sha origin/main 2>$dev) { "YES" } else { "NO" }',
    );
    expect(v.join()).toContain("powershell-if-native-command");
  });

  // Review on #1988: requiring `)` or `{` immediately after the call missed the
  // standard PowerShell spellings, so the trap walked straight through a rule
  // that looked like coverage. Each case below is a real idiom, not a synthetic
  // shape invented for the regex.
  it("blocks the Allman brace style (opening brace on the next line)", () => {
    const v = checkContentViolations(
      "scripts/probe.ps1",
      'if (git diff --quiet)\n{\n    Write-Output "unchanged"\n}',
    );
    expect(v.join()).toContain("powershell-if-native-command");
  });

  it("blocks a trailing comment after the condition", () => {
    const v = checkContentViolations(
      "scripts/probe.ps1",
      'if (git diff --quiet) # is the worktree clean?\n{\n    Write-Output "unchanged"\n}',
    );
    expect(v.join()).toContain("powershell-if-native-command");
  });

  it("blocks `elseif (...)` and the two-word `else if (...)`", () => {
    for (const line of [
      'elseif (git diff --quiet) { Write-Output "clean" }',
      'else if (git diff --quiet) { Write-Output "clean" }',
    ]) {
      expect(
        checkContentViolations("scripts/probe.ps1", line).join(),
        line,
      ).toContain("powershell-if-native-command");
    }
  });

  it("blocks the idiomatic `-not` negation", () => {
    for (const line of [
      'if (-not (git diff --quiet)) { Write-Output "dirty" }',
      'if (-not(git diff --quiet)) { Write-Output "dirty" }',
    ]) {
      expect(
        checkContentViolations("scripts/probe.ps1", line).join(),
        line,
      ).toContain("powershell-if-native-command");
    }
  });

  it("blocks a native command continued into a compound condition", () => {
    for (const line of [
      'if (git diff --quiet) -and $dirty { Write-Output "x" }',
      'if (git diff --quiet) -or $flag { Write-Output "x" }',
    ]) {
      expect(
        checkContentViolations("scripts/probe.ps1", line).join(),
        line,
      ).toContain("powershell-if-native-command");
    }
  });

  // Pinned limit, not an oversight: a native call in a *later* clause of a
  // compound condition is not matched. Matching it means allowing arbitrary
  // text before the command, which also flags legitimate comparisons such as
  // `if ($env:PATH -like "*git *")`. Prefer a false negative a reviewer can see
  // over a false positive that blocks correct code — the limit is documented in
  // the rule and in docs/agents/engineering.md.
  it("does NOT reach a native call in a later clause of a compound condition", () => {
    const v = checkContentViolations(
      "scripts/probe.ps1",
      'if ($dirty -or (git diff --quiet)) { Write-Output "x" }',
    );
    expect(v).toHaveLength(0);
  });

  it("blocks other silent native commands used as a bare condition", () => {
    const cases = [
      'if (git diff --quiet) { "unchanged" }',
      'if (!(gh pr checks 1977)) { "none" }',
      'while (cargo fmt --check) { Start-Sleep 1 }',
      'if (npm run lint) { "clean" }',
    ];
    for (const line of cases) {
      expect(
        checkContentViolations("scripts/probe.ps1", line).join(),
        line,
      ).toContain("powershell-if-native-command");
    }
  });

  it("allows the $LASTEXITCODE form", () => {
    const v = checkContentViolations(
      "scripts/check-merge.ps1",
      "git merge-base --is-ancestor $sha origin/main 2>$dev\nif ($LASTEXITCODE -eq 0) { \"YES\" } else { \"NO\" }",
    );
    expect(v).toHaveLength(0);
  });

  it("allows a cmdlet that returns a value (`Test-Path`)", () => {
    const v = checkContentViolations(
      "scripts/run.ps1",
      "if (Test-Path $logPath) { Get-Content $logPath -Tail 40 }",
    );
    expect(v).toHaveLength(0);
  });

  it("allows explicit comparisons of a native command's output", () => {
    // The widened terminator (end-of-line / comment) must not start matching
    // these: `.Length`, `.Count` and `-gt` all follow the call, not `)`+brace.
    for (const line of [
      "if ((git status --porcelain).Length -gt 0) { Write-Output 'dirty' }",
      "if ((git status --porcelain).Length -gt 0)\n{\n    Write-Output 'dirty'\n}",
      "if (@(git ls-files).Count -gt 0) { Write-Output 'has files' }",
      "if ($diff.Length -gt 0) { Write-Output 'has output' }",
    ]) {
      expect(checkContentViolations("scripts/probe.ps1", line).join(), line).toBe(
        "",
      );
    }
  });

  it("allows the `# allow-native-condition` escape hatch", () => {
    const v = checkContentViolations(
      "scripts/probe.ps1",
      'if (git branch --show-current) { "on a branch" } # allow-native-condition',
    );
    expect(v).toHaveLength(0);
  });

  it("does NOT police markdown, where the wrong form is quoted on purpose", () => {
    const v = checkContentViolations(
      "docs/agents/engineering.md",
      'if (git merge-base --is-ancestor $sha origin/main 2>$dev) { "YES" } else { "NO" }',
    );
    expect(v).toHaveLength(0);
  });

  it("does NOT police TypeScript or Rust", () => {
    expect(
      checkContentViolations(
        "src/components/Terminal.tsx",
        "if (git status --porcelain) {}",
      ),
    ).toHaveLength(0);
    expect(
      checkContentViolations(
        "src-tauri/src/agent/spawn.rs",
        "if git status --porcelain {}",
      ),
    ).toHaveLength(0);
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
