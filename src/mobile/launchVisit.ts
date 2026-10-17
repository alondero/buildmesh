/**
 * Navigation authority for launch/resume completions (issue #2327).
 *
 * Creating an Agent Node from a GitHub Issue or resuming an archived session
 * spawns a real agent, so the request can stay pending for minutes. The screen
 * that started it is not necessarily still on display when it resolves: the
 * user may have pressed Back and opened a different Mesh or Agent Node.
 *
 * A component's unmount is not that decision — it only tells the screen it may
 * stop writing. The screens own asynchronous work whose backend half must be
 * allowed to finish (the worktree, the process, the session) while only the
 * *presentation* half — the navigation to the result — belongs to the visit that
 * started it. So each visit carries a stable identity, captured when the
 * mutation starts, and the App re-checks it at the navigation transition rather
 * than trusting each screen to have retired its own work.
 *
 * Both launch screens share this record but keep their operations separate:
 * `IssuesScreen` spawns from an issue, `ArchivedNodesScreen` resumes a session.
 * Only the ownership check is common.
 */

/** The screen kinds that own a launch/resume completion. */
type LaunchVisitKind = "issues" | "sessions";

/**
 * Identity of one visit to a launch screen.
 *
 * `visitId` is minted per visit, never per screen: returning to the same Mesh's
 * Issues screen creates a new visit rather than restoring the authority of the
 * request it replaced. `meshId` is part of the identity too, so a completion
 * cannot be applied to a different Mesh's screen.
 */
export type LaunchVisit = {
  kind: LaunchVisitKind;
  meshId: number;
  visitId: number;
};

/**
 * Whether `visit` — the identity captured when a mutation started — is still
 * the launch screen on display.
 *
 * `current` is the visit the App can read off whatever it is rendering, or
 * `null` when the visible screen is not a launch screen at all (the node list,
 * a node's Overview/Terminal, Connect during auth recovery). A retired visit
 * therefore fails this check, and its completion has nowhere to navigate to —
 * the acknowledged backend work still happened and stays discoverable through
 * the node list's own refresh.
 */
export function launchVisitIsCurrent(
  current: LaunchVisit | null,
  visit: LaunchVisit,
): boolean {
  return (
    current !== null &&
    current.kind === visit.kind &&
    current.meshId === visit.meshId &&
    current.visitId === visit.visitId
  );
}