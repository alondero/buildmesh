/**
 * `src/lib/archiveProvenance.ts` — the vocabulary bridge between an archive
 * entry's provenance (an adapter id) and the Spawn Menu's rows (Harness
 * Profile ids, plus Proxied Provider composites). Issue #2332.
 *
 * These cases are the ones a wrong answer would misroute a resume through:
 * the `claude` → `anthropic` alias, a proxied id, and an unrecognised id.
 */
import { describe, expect, it } from "vitest";

import {
  adapterIdFromHarnessId,
  adapterIdFromProvider,
  archiveHarnessLabel,
  harnessIdFromProvider,
  resumeOptionForHarness,
  resumeProviderForHarness,
} from "../../src/lib/archiveProvenance";

type Row = {
  id: string;
  harness_id: string;
  is_proxied: boolean;
};

const ROWS: Row[] = [
  { id: "claude", harness_id: "claude", is_proxied: false },
  { id: "claude:minimax", harness_id: "claude", is_proxied: true },
  { id: "cursor", harness_id: "cursor", is_proxied: false },
  { id: "commandcode", harness_id: "commandcode", is_proxied: false },
  { id: "terminal", harness_id: "terminal", is_proxied: false },
];

describe("adapterIdFromHarnessId", () => {
  it("resolves the built-in Harness Profile alias", () => {
    expect(adapterIdFromHarnessId("claude")).toBe("anthropic");
  });

  it("passes an adapter id through unchanged", () => {
    expect(adapterIdFromHarnessId("cursor")).toBe("cursor");
  });

  it("returns an unknown id unchanged rather than undefined", () => {
    // A future harness this build does not know must not collapse into
    // another harness's executor.
    expect(adapterIdFromHarnessId("harness-from-the-future")).toBe("harness-from-the-future");
  });
});

describe("adapterIdFromProvider", () => {
  it("resolves a proxied provider id to its harness's adapter", () => {
    expect(adapterIdFromProvider("claude:minimax")).toBe("anthropic");
    expect(adapterIdFromProvider("claude")).toBe("anthropic");
  });

  it("resolves a bare adapter id to itself", () => {
    expect(adapterIdFromProvider("agy")).toBe("agy");
  });
});

describe("harnessIdFromProvider", () => {
  it("keeps only the harness half of a composite id", () => {
    expect(harnessIdFromProvider("claude:minimax")).toBe("claude");
  });

  it("returns a bare id unchanged", () => {
    expect(harnessIdFromProvider("cursor")).toBe("cursor");
  });
});

describe("archiveHarnessLabel", () => {
  it("names the harness from an adapter id", () => {
    expect(archiveHarnessLabel("anthropic")).toBe("Claude Code");
    expect(archiveHarnessLabel("cursor")).toBe("Cursor");
  });

  it("names the harness behind a proxied provider id", () => {
    // A composite id has no single label of its own; the harness it runs on
    // is the honest answer.
    expect(archiveHarnessLabel("claude:minimax")).toBe("Claude Code");
  });

  it("returns null for absent provenance", () => {
    // `null` is what makes a surface show the unavailable state instead of
    // claiming Claude Code created the conversation.
    expect(archiveHarnessLabel(null)).toBeNull();
    expect(archiveHarnessLabel(undefined)).toBeNull();
    expect(archiveHarnessLabel("   ")).toBeNull();
  });

  it("falls back to the id itself when the harness has no label", () => {
    expect(archiveHarnessLabel("harness-from-the-future")).toBe("harness-from-the-future");
  });
});

describe("resumeOptionForHarness", () => {
  it("picks the native row for the conversation's harness", () => {
    expect(resumeOptionForHarness(ROWS, "cursor")?.id).toBe("cursor");
    expect(resumeOptionForHarness(ROWS, "anthropic")?.id).toBe("claude");
  });

  it("matches a proxied provenance to its native row", () => {
    expect(resumeOptionForHarness(ROWS, "claude:minimax")?.id).toBe("claude");
  });

  it("never picks a proxied row as the harness's executor", () => {
    // `claude:minimax` is a Model Provider choice inside Claude Code, not a
    // second Claude Code executor; picking it would make the default depend
    // on menu ordering.
    const chosen = resumeOptionForHarness(ROWS, "anthropic");
    expect(chosen?.is_proxied).toBe(false);
  });

  it("returns undefined when the harness has no row (not installed)", () => {
    expect(resumeOptionForHarness(ROWS, "agy")).toBeUndefined();
  });

  it("returns undefined for absent provenance", () => {
    expect(resumeOptionForHarness(ROWS, null)).toBeUndefined();
    expect(resumeOptionForHarness(ROWS, undefined)).toBeUndefined();
  });
});

describe("resumeProviderForHarness", () => {
  it("is the row id when the harness is available", () => {
    expect(resumeProviderForHarness(ROWS, "commandcode")).toBe("commandcode");
  });

  it("is null when it cannot be resolved, so callers cannot fall back to a guess", () => {
    expect(resumeProviderForHarness(ROWS, "agy")).toBeNull();
    expect(resumeProviderForHarness(ROWS, null)).toBeNull();
  });
});