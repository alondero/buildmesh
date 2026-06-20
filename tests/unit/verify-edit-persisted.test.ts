import { describe, it, expect } from "vitest";
import {
  checkRecentlyWritten,
  FRESH_WINDOW_MS,
} from "../../.claude/hooks/verify-edit-persisted.mjs";

const FILE = "X:\\src\\buildmesh\\.claude\\worktrees\\red-rare-hedge\\src\\App.tsx";
const NOW = 1_000_000_000_000; // fixed clock for determinism
const statAt = (mtimeMs: number) => () => ({ mtimeMs });

describe("checkRecentlyWritten", () => {
  it("returns null when the file was written just now (mtime == now)", () => {
    expect(checkRecentlyWritten(FILE, statAt(NOW), NOW)).toBeNull();
  });

  it("returns null for a write a few seconds ago (inside the window)", () => {
    expect(checkRecentlyWritten(FILE, statAt(NOW - 3000), NOW)).toBeNull();
  });

  it("warns when the file's mtime is older than the window (edit never landed)", () => {
    const msg = checkRecentlyWritten(FILE, statAt(NOW - 60_000), NOW);
    expect(msg).toContain("[edit-not-persisted]");
    expect(msg).toContain(FILE);
    expect(msg).toContain("~60s ago");
  });

  it("treats the window edge as fresh (not a warning)", () => {
    expect(checkRecentlyWritten(FILE, statAt(NOW - FRESH_WINDOW_MS), NOW)).toBeNull();
  });

  it("is encoding-agnostic — never reads file contents, so em-dashes can't false-fire", () => {
    // A file full of em-dashes that DID persist (fresh mtime) must not warn.
    // This is the regression the content-substring version failed.
    expect(checkRecentlyWritten(FILE, statAt(NOW - 100), NOW)).toBeNull();
  });

  it("warns on ENOENT — a new-file write that didn't land leaves no file", () => {
    const stat = () => {
      const e: NodeJS.ErrnoException = new Error("no such file");
      e.code = "ENOENT";
      throw e;
    };
    const msg = checkRecentlyWritten(FILE, stat, NOW);
    expect(msg).toContain("[edit-not-persisted]");
    expect(msg).toContain("no file exists");
  });

  it("fails open on a non-ENOENT stat error (e.g. permissions)", () => {
    const stat = () => {
      const e: NodeJS.ErrnoException = new Error("permission denied");
      e.code = "EPERM";
      throw e;
    };
    expect(checkRecentlyWritten(FILE, stat, NOW)).toBeNull();
  });

  it("fails open on an empty file path", () => {
    expect(checkRecentlyWritten("", statAt(0), NOW)).toBeNull();
  });
});
