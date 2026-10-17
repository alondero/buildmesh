/**
 * Launch/resume navigation ownership (issue #2327).
 *
 * Creating an Agent Node from an issue or resuming an archived session spawns a
 * real agent, so the completion arrives long after the tap. The question these
 * tests answer is the whole contract: *may* this completion still take the user
 * to its result? A visit that is no longer on display must not, whatever the
 * screen that started it still believes.
 */
import { describe, it, expect } from "vitest";
import { launchVisitIsCurrent, type LaunchVisit } from "../../src/mobile/launchVisit";

const issuesVisit: LaunchVisit = { kind: "issues", meshId: 1, visitId: 7 };
const sessionsVisit: LaunchVisit = { kind: "sessions", meshId: 1, visitId: 7 };

describe("launchVisitIsCurrent", () => {
  it("accepts the visit that is still on screen", () => {
    expect(launchVisitIsCurrent(issuesVisit, issuesVisit)).toBe(true);
    expect(launchVisitIsCurrent(sessionsVisit, sessionsVisit)).toBe(true);
  });

  it("rejects when no launch screen is displayed at all", () => {
    // The user pressed Back and landed on the node list, or on an Agent Node's
    // Overview / Terminal, or on Connect after an auth recovery.
    expect(launchVisitIsCurrent(null, issuesVisit)).toBe(false);
    expect(launchVisitIsCurrent(null, sessionsVisit)).toBe(false);
  });

  it("rejects a second visit to the same Mesh's screen", () => {
    // Returning to Issues for Mesh 1 mints a new visit id, so the request the
    // user walked away from cannot claim the screen they came back to.
    expect(launchVisitIsCurrent({ ...issuesVisit, visitId: 8 }, issuesVisit)).toBe(
      false,
    );
    expect(
      launchVisitIsCurrent({ ...sessionsVisit, visitId: 8 }, sessionsVisit),
    ).toBe(false);
  });

  it("rejects a different Mesh, even at the same visit id", () => {
    expect(
      launchVisitIsCurrent({ ...issuesVisit, meshId: 2 }, issuesVisit),
    ).toBe(false);
  });

  it("rejects the other launch screen's visit", () => {
    // Issues and Archive are different visits even for the same Mesh and the
    // same id: a resume completion must not navigate an Issues screen.
    expect(launchVisitIsCurrent(sessionsVisit, issuesVisit)).toBe(false);
    expect(launchVisitIsCurrent(issuesVisit, sessionsVisit)).toBe(false);
  });
});