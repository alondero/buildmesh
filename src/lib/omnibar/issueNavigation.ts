export interface IssueNavigationRequest {
  meshId: number;
  issueNumber: number;
}

type IssueNavigationListener = (request: IssueNavigationRequest) => void;

let pendingRequest: IssueNavigationRequest | null = null;
let listener: IssueNavigationListener | null = null;

/** Queue an issue navigation until the Issues tab has mounted. */
export function requestIssueNavigation(request: IssueNavigationRequest): void {
  if (listener !== null) {
    listener(request);
    return;
  }
  pendingRequest = request;
}

/** Register the mounted Issues tab and consume one queued navigation. */
export function registerIssueNavigation(next: IssueNavigationListener): () => void {
  listener = next;
  const queued = pendingRequest;
  pendingRequest = null;
  if (queued !== null) next(queued);
  return () => {
    if (listener === next) listener = null;
  };
}

/** Test isolation for the one-shot navigation handoff. */
export function resetIssueNavigationForTests(): void {
  pendingRequest = null;
  listener = null;
}
