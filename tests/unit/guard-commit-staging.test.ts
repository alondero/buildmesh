import { describe, it, expect } from "vitest";
import { classifyCommand, decide } from "../../.claude/hooks/guard-commit-staging.mjs";

describe("classifyCommand", () => {
  const plain = (c: string) => classifyCommand(c);

  it("flags a bare commit as a plain commit", () => {
    expect(plain("git commit -m 'fix thing'")).toEqual({ isCommit: true, isPlainCommit: true });
    expect(plain("git commit -F msg.txt")).toEqual({ isCommit: true, isPlainCommit: true });
    expect(plain("git commit")).toEqual({ isCommit: true, isPlainCommit: true });
  });

  it("does NOT treat inline-staging commits as plain (can't judge pre-add state)", () => {
    expect(plain("git add -A && git commit -m x").isPlainCommit).toBe(false);
    expect(plain("git add src/a.ts; git commit -m x").isPlainCommit).toBe(false);
  });

  it("skips -a/--all/-am, --amend, --allow-empty", () => {
    expect(plain("git commit -am 'x'").isPlainCommit).toBe(false);
    expect(plain("git commit --all -m 'x'").isPlainCommit).toBe(false);
    expect(plain("git commit --amend --no-edit").isPlainCommit).toBe(false);
    expect(plain("git commit --allow-empty -m 'x'").isPlainCommit).toBe(false);
  });

  it("is not a commit for unrelated commands", () => {
    expect(plain("git status").isCommit).toBe(false);
    expect(plain("git push origin main").isCommit).toBe(false);
    expect(plain("echo hello world").isCommit).toBe(false);
    expect(plain("npm run build").isCommit).toBe(false);
  });

  it("detects a commit after a shell separator", () => {
    expect(plain("cd repo && git commit -m x")).toEqual({ isCommit: true, isPlainCommit: true });
  });

  it("is not fooled by another command's a-flag in a compound (#1)", () => {
    // `ls -la` must NOT make the plain commit look auto-staged.
    expect(plain("ls -la && git commit -m 'wip'")).toEqual({ isCommit: true, isPlainCommit: true });
  });

  it("is not fooled by 'git add' inside the commit MESSAGE (#2)", () => {
    expect(plain("git commit -m \"run git add first\"")).toEqual({
      isCommit: true,
      isPlainCommit: true,
    });
  });

  it("recognises `git -C <path> commit` (#3)", () => {
    expect(plain("git -C /path/to/repo commit -m x")).toEqual({
      isCommit: true,
      isPlainCommit: true,
    });
  });

  it("does NOT treat `git push origin commit-branch` as a commit (no false block)", () => {
    expect(plain("git push origin commit-branch").isCommit).toBe(false);
  });

  it("skips when a staging command runs earlier in a subshell chain", () => {
    expect(plain("(cd x && git add .) && git commit -m y").isPlainCommit).toBe(false);
  });
});

describe("decide", () => {
  const stateWith = (stagedCount: number, statusShort = "") => () => ({ stagedCount, statusShort });

  it("denies a plain commit with nothing staged", () => {
    const v = decide("git commit -m 'fix'", stateWith(0, " M src/a.ts\n M src/b.ts"));
    expect(v?.permissionDecision).toBe("deny");
    expect(v?.permissionDecisionReason).toContain("Nothing is staged");
    expect(v?.permissionDecisionReason).toContain("src/a.ts"); // shows the working tree
  });

  it("allows a plain commit that has staged changes", () => {
    expect(decide("git commit -m 'fix'", stateWith(3))).toBeNull();
  });

  it("denies an empty commit even when chained after an unrelated a-flagged command (#1)", () => {
    const v = decide("ls -la && git commit -m wip", stateWith(0));
    expect(v?.permissionDecision).toBe("deny");
  });

  it("allows (skips) an add && commit even when currently nothing is staged", () => {
    // The add hasn't run yet at PreToolUse time, so we must not judge it.
    expect(decide("git add -A && git commit -m x", stateWith(0))).toBeNull();
  });

  it("allows --allow-empty commits", () => {
    expect(decide("git commit --allow-empty -m x", stateWith(0))).toBeNull();
  });

  it("allows non-commit commands without consulting git state", () => {
    let called = false;
    const provider = () => {
      called = true;
      return { stagedCount: 0, statusShort: "" };
    };
    expect(decide("git status", provider)).toBeNull();
    expect(called).toBe(false);
  });

  it("fails open if reading git state throws (e.g. not a repo)", () => {
    const provider = () => {
      throw new Error("not a git repository");
    };
    expect(decide("git commit -m x", provider)).toBeNull();
  });
});
