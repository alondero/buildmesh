/**
 * `FeedCompletenessNote` — the visible half of issue #1528 / #2024 rank 6.
 *
 * The backend now reports whether a paginated GitHub read got everything
 * (`services::github::pagination`). This is the component that turns that into
 * something a user can act on, so the copy is part of the contract: it must
 * never imply a complete read it cannot prove, and it must distinguish a
 * permanent limit (safety cap, search ceiling) from a transient one (upstream
 * incompleteness, cancellation), because the remedy differs.
 */
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { FeedCompletenessNote } from "../../src/components/Probe/FeedCompletenessNote";
import type { GitHubPageCompleteness } from "../../src/types/generated/GitHubPageCompleteness";

function completeness(overrides: Partial<GitHubPageCompleteness> = {}): GitHubPageCompleteness {
  return {
    returned: 100,
    pages_fetched: 1,
    complete: false,
    incomplete_reason: "safety_cap",
    reported_total: null,
    ...overrides,
  };
}

describe("FeedCompletenessNote", () => {
  it("renders nothing for a complete read", () => {
    const { container } = render(
      <FeedCompletenessNote
        completeness={completeness({ complete: true, incomplete_reason: null })}
        label="issues"
      />,
    );
    // A complete read is the overwhelmingly common case; it must not spend
    // vertical space or imply a problem that does not exist.
    expect(container.textContent).toBe("");
  });

  it("renders nothing before the first read lands", () => {
    const { container } = render(
      <FeedCompletenessNote completeness={null} label="issues" />,
    );
    expect(container.textContent).toBe("");
  });

  it("names the count and the reason when GitHub reports no total", () => {
    render(<FeedCompletenessNote completeness={completeness()} label="issues" />);
    const note = screen.getByTestId("feed-completeness-note").textContent ?? "";
    expect(note).toContain("Showing the first 100 issues");
    expect(note).toContain("page limit");
  });

  it("shows 'first N of M' when GitHub reported a total", () => {
    render(
      <FeedCompletenessNote
        completeness={completeness({
          incomplete_reason: "search_ceiling",
          reported_total: 1240,
        })}
        label="issues"
      />,
    );
    const note = screen.getByTestId("feed-completeness-note").textContent ?? "";
    expect(note).toContain("Showing the first 100 of 1240 issues");
    expect(note).toContain("1,000");
  });

  it("tells a transient upstream failure apart from a permanent cap", () => {
    const { unmount } = render(
      <FeedCompletenessNote
        completeness={completeness({ incomplete_reason: "upstream_incomplete" })}
        label="pull requests"
      />,
    );
    expect(screen.getByTestId("feed-completeness-note").textContent).toContain(
      "try again",
    );
    unmount();

    render(
      <FeedCompletenessNote
        completeness={completeness({ incomplete_reason: "search_ceiling" })}
        label="pull requests"
      />,
    );
    expect(screen.getByTestId("feed-completeness-note").textContent).not.toContain(
      "try again",
    );
  });

  it("reports a cancelled read as cancelled, not as a truncation", () => {
    render(
      <FeedCompletenessNote
        completeness={completeness({
          returned: 0,
          pages_fetched: 0,
          incomplete_reason: "cancelled",
        })}
        label="changed files"
      />,
    );
    const note = screen.getByTestId("feed-completeness-note").textContent ?? "";
    expect(note).toContain("cancelled");
    expect(note).toContain("Showing the first 0 changed files");
  });
});